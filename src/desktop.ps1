# Fixed read-only helper. Input is JSON data; no user text is evaluated as code.
[Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
try {
    $requestData = [Console]::In.ReadToEnd()
    if ($requestData.Length -gt 4096) { throw 'Invalid request' }
    $inputData = $requestData | ConvertFrom-Json
    $Read = $inputData.operation -eq 'read'
    if (($inputData.operation -ne 'probe' -and -not $Read) -or ($inputData.port -isnot [int] -and $inputData.port -isnot [long]) -or
        $inputData.port -lt 1024 -or $inputData.port -gt 65535) { throw 'Invalid request' }
    $Port = [int]$inputData.port
    if ($Read) {
        # Parameters reach the page only as a JSON literal built here.
        $id = '^[A-Za-z0-9_.:@-]{1,256}$'
        $after = $inputData.after
        if ($inputData.query -notin @('chats','messages') -or $inputData.limit -isnot [int] -or
            $inputData.limit -lt 1 -or $inputData.limit -gt 200 -or
            ($inputData.query -eq 'messages' -and -not ([string]$inputData.chat_id).StartsWith('19:')) -or
            ($inputData.query -eq 'messages' -and [string]$inputData.chat_id -cnotmatch $id) -or
            ($null -ne $after -and [string]$after.id -cnotmatch $id)) { throw 'Invalid request' }
        $params = [ordered]@{ query = [string]$inputData.query; chat = [string]$inputData.chat_id; limit = [int]$inputData.limit; after = $null }
        if ($null -ne $after) { $params.after = [ordered]@{ t = [long]$after.t; id = [string]$after.id } }
    }
} catch {
    [Console]::Out.WriteLine('{"error":{"kind":"invalid_input","message":"Expected a probe or read request with a port between 1024 and 65535"}}')
    exit 1
}

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Read-LocalJson([string]$Route) {
    $request = [Net.HttpWebRequest]::Create("http://127.0.0.1:$Port$Route")
    $request.Proxy = $null
    $request.AllowAutoRedirect = $false
    $request.Timeout = 5000
    $request.ReadWriteTimeout = 5000
    $request.UserAgent = 'TeamsCLI-DesktopProbe/0.1'
    $response = $request.GetResponse()
    try {
        if ([int]$response.StatusCode -ne 200) { throw 'Unexpected HTTP status' }
        $reader = New-Object IO.StreamReader($response.GetResponseStream())
        try {
            $buffer = New-Object char[] 1048577
            $length = $reader.ReadBlock($buffer, 0, $buffer.Length)
            if ($length -eq 0 -or $length -gt 1048576) { throw 'Invalid metadata size' }
            $document = -join $buffer[0..($length-1)]
            $start = $document.TrimStart()
            if (($Route -eq '/json/version' -and -not $start.StartsWith('{')) -or
                ($Route -eq '/json/list' -and -not $start.StartsWith('['))) { throw 'Invalid metadata shape' }
            $parsed = $document | ConvertFrom-Json
            return $parsed
        } finally { $reader.Dispose() }
    } finally { $response.Dispose() }
}

function Test-TeamsOwner([int]$OwnerId) {
    # Process ancestry only; never read command lines, titles, caches or credentials.
    $seen = @{}
    for ($i = 0; $i -lt 32 -and $OwnerId -gt 0; $i++) {
        if ($seen.ContainsKey($OwnerId)) { break }
        $seen[$OwnerId] = $true
        $process = Get-CimInstance Win32_Process -Filter "ProcessId=$OwnerId"
        if (-not $process) { break }
        if ($process.Name -ieq 'ms-teams.exe') {
            if ($script:teamsExecutables -inotcontains $process.ExecutablePath) { return $false }
            $owner = Invoke-CimMethod -InputObject $process -MethodName GetOwnerSid
            return $owner.Sid -eq $script:currentSid
        }
        $OwnerId = [int]$process.ParentProcessId
    }
    return $false
}

$result = [ordered]@{
    schema = 'teams-desktop-metadata-v1'
    checked_utc = [DateTime]::UtcNow.ToString('o')
    port = $Port
    status = 'no_listener'
    loopback_only = $null
    teams_owned = $null
    version = $null
    browser_websocket_advertised = $false
    targets = @()
}
try {
    $script:currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $script:teamsExecutables = @(Get-AppxPackage -Name MSTeams | ForEach-Object {
        Join-Path $_.InstallLocation 'ms-teams.exe'
    })
    $listeners = @(Get-NetTCPConnection -State Listen -ErrorAction Stop |
        Where-Object LocalPort -eq $Port)
    if ($listeners.Count) {
        $result.loopback_only = @($listeners | Where-Object {
            $_.LocalAddress -notin @('127.0.0.1','::1')
        }).Count -eq 0
        $owned = @($listeners | ForEach-Object { Test-TeamsOwner $_.OwningProcess })
        $result.teams_owned = ($owned -notcontains $false)
        if (-not $result.loopback_only) { $result.status = 'unsafe_listener_refused' }
        elseif (-not $result.teams_owned) { $result.status = 'non_teams_listener_refused' }
        else {
            $version = Read-LocalJson '/json/version'
            $targets = @(Read-LocalJson '/json/list')
            $result.version = [ordered]@{
                browser = [string]$version.Browser
                protocol = [string]$version.'Protocol-Version'
            }
            $result.browser_websocket_advertised = [bool]$version.webSocketDebuggerUrl
            $result.targets = @($targets | ForEach-Object {
                $isTeams = $false
                $origin = '[other origin omitted]'
                $scheme = ''
                try {
                    $uri = [Uri]$_.url
                    $scheme = $uri.Scheme
                    $isTeams = ($uri.Scheme -eq 'https' -and $uri.DnsSafeHost -in @('teams.microsoft.com','teams.cloud.microsoft','teams.live.com') -and $uri.Port -eq 443)
                    if ($isTeams -or ($uri.Scheme -eq 'https' -and $uri.DnsSafeHost -in @('login.microsoftonline.com','login.live.com') -and $uri.Port -eq 443)) {
                        $origin = 'https://' + $uri.DnsSafeHost
                    } elseif ($uri.Scheme -in @('file','blob','about')) { $origin = $uri.Scheme + ':' }
                } catch { }
                # No target titles, URLs, IDs or WebSocket URLs leave this function.
                [ordered]@{
                    type = [string]$_.type
                    url_scheme = $scheme
                    url_origin = $origin
                    teams_origin = $isTeams
                    websocket_advertised = [bool]$_.webSocketDebuggerUrl
                }
            })
            $result.status = 'metadata_ok'
        }
    }
} catch { $result.status = 'metadata_probe_failed' }
if (-not $Read) { $result | ConvertTo-Json -Depth 8 -Compress; exit }
if ($result.status -ne 'metadata_ok') { '{"error":"' + $result.status + '"}'; exit }
# Projects the page's own IndexedDB cache; raw records never leave the page.
$readScript = @'
async p=>{const all=await indexedDB.databases(),req=q=>new Promise((r,j)=>{q.onsuccess=()=>r(q.result);q.onerror=()=>j(q.error)}),
open=k=>{const d=all.find(x=>x.name.includes(':'+k+':'));if(!d)throw Error('layout');return req(indexedDB.open(d.name))},
iso=t=>t?new Date(+t).toISOString():null,uid=m=>(m||'').replace(/^.*\//,'').replace(/^8:orgid:/,''),sys=t=>/^(ThreadActivity|Event|Control)\//.test(t||''),
page=rows=>{rows.sort((a,b)=>b.t-a.t||(a.id<b.id?-1:a.id>b.id?1:0));const c=p.after;if(c)rows=rows.filter(r=>r.t<c.t||r.t==c.t&&r.id>c.id);
const items=rows.slice(0,p.limit),last=items[items.length-1];return{items,next:rows.length>p.limit?{t:last.t,id:last.id}:null}},
prof=async ids=>{const n={};let d;try{d=await open('profiles');const s=d.transaction('profiles').objectStore('profiles');
await Promise.all(ids.map(async i=>{const r=await req(s.get('8:orgid:'+i));if(r&&r.displayName)n[i]=r.displayName}))}catch(e){}finally{d&&d.close()}return n},
chain=async(d,id)=>(await req(d.transaction('replychains').objectStore('replychains').getAll(IDBKeyRange.bound([id],[id,[]])))).flatMap(rc=>Object.values(rc.messageMap||{}));
const cdb=await open('conversation-manager'),me=cdb.name.split(':')[4],cs=cdb.transaction('conversations').objectStore('conversations');let pg;
try{if(p.query=='chats')pg=page((await req(cs.getAll())).filter(c=>c.type=='Chat'||c.type=='Meeting').map(c=>({t:c.lastMessageTimeUtc||0,id:c.id,c})));
else if(!await req(cs.get(p.chat)))return{error:'not_found'}}finally{cdb.close()}
const rdb=await open('replychain-manager');try{if(pg){
const cs=pg.items.map(({c})=>{const lm=c.lastMessage||{},one=/@unq\.gbl\.spaces$/.test(c.id),nm={},h=+((c.properties||{}).consumptionhorizon||'').split(';')[0],
mem=new Set((c.members||[]).map(m=>uid(m.id)).concat(one?c.id.slice(3).split('@')[0].split('_'):[]));if(lm.imdisplayname)nm[uid(lm.from)]=lm.imdisplayname;
return{c,one,mem,nm,v:{id:c.id,topic:(c.threadProperties||{}).topic||null,chatType:c.type=='Meeting'?'meeting':one?'oneOnOne':'group',
lastUpdatedDateTime:iso(c.lastMessageTimeUtc),viewpoint:{lastMessageReadDateTime:h?iso(h):null},lastMessagePreview:lm.id?{id:lm.id,
createdDateTime:iso(Date.parse(lm.originalarrivaltime)||lm.id),isDeleted:!!lm.deletionInfo,messageType:sys(lm.messagetype)?'systemEventMessage':'message',
from:{user:{id:uid(lm.from),displayName:lm.imdisplayname||null}},body:{contentType:/^RichText/.test(lm.messagetype)?'html':'text',content:lm.content||''}}:null}}}),
n=await prof([...new Set(cs.flatMap(x=>[...x.mem]))]);
for(const x of cs){const who=()=>[...x.mem].filter(i=>i!=me&&!n[i]&&!x.nm[i]);if(x.one&&(who().length||x.mem.size<2))for(const m of await chain(rdb,x.c.id)){const i=uid(m.creator);
if(i&&i!=me&&m.imDisplayName){x.mem.add(i);x.nm[i]=x.nm[i]||m.imDisplayName}if(!who().length&&x.mem.size>1)break}
x.v.members=[...x.mem].filter(i=>i).map(i=>({userId:i,displayName:n[i]||x.nm[i]||null}))}
return{me,items:cs.map(x=>x.v),next:pg.next}}
const rows=[],seen={};for(const m of await chain(rdb,p.chat)){if(sys(m.messageType)||seen[m.id])continue;seen[m.id]=1;
rows.push({t:m.originalArrivalTime||0,id:m.id,v:{id:m.id,createdDateTime:iso(m.originalArrivalTime),from:{user:{id:uid(m.creator),displayName:m.imDisplayName||null}},
body:{contentType:/^RichText/.test(m.messageType)?'html':'text',content:m.content||''},deleted:!!m.deletionInfo}})}
pg=page(rows);const n=await prof([...new Set(pg.items.map(r=>r.v.from.user).filter(u=>!u.displayName).map(u=>u.id))]);
for(const r of pg.items)r.v.from.user.displayName=r.v.from.user.displayName||n[r.v.from.user.id]||null;return{me,items:pg.items.map(r=>r.v),next:pg.next}}finally{rdb.close()}}
'@
$page = @($targets | Where-Object {
    $_.type -eq 'page' -and $_.url -match '^https://teams\.(microsoft\.com|cloud\.microsoft)/' -and $_.id -match '^[A-Fa-f0-9]{8,64}$'
})[0]
if (-not $page) { '{"error":"no_target"}'; exit }
$socket = New-Object Net.WebSockets.ClientWebSocket
$cancel = New-Object Threading.CancellationTokenSource 30000
try {
    # The socket URL is rebuilt from the verified port, never taken from the endpoint.
    $socket.ConnectAsync([Uri]"ws://127.0.0.1:$Port/devtools/page/$($page.id)", $cancel.Token).Wait()
    $expression = '(' + $readScript + ')(' + ($params | ConvertTo-Json -Depth 4 -Compress) + ').then(JSON.stringify)'
    $message = [Text.Encoding]::UTF8.GetBytes((@{ id = 1; method = 'Runtime.evaluate'; params = @{
        expression = $expression; awaitPromise = $true; returnByValue = $true } } | ConvertTo-Json -Depth 4 -Compress))
    $socket.SendAsync((New-Object ArraySegment[byte] -ArgumentList (,$message)), 'Text', $true, $cancel.Token).Wait()
    $buffer = New-Object byte[] 65536
    $reply = New-Object IO.MemoryStream
    do {
        $reply.SetLength(0)
        do {
            $received = $socket.ReceiveAsync((New-Object ArraySegment[byte] -ArgumentList (,$buffer)), $cancel.Token).Result
            $reply.Write($buffer, 0, $received.Count)
            if ($reply.Length -gt 8MB) { throw 'Reply too large' }
        } until ($received.EndOfMessage)
        $text = [Text.Encoding]::UTF8.GetString($reply.ToArray())
    } until ($text.StartsWith('{"id":1,'))
    $text
} catch { '{"error":"read_failed"}' } finally { $socket.Dispose(); $cancel.Dispose() }
