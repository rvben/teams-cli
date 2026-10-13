// Projects the Teams page's own IndexedDB cache; raw records never leave the page.
// Evaluated as `(script)(params)`, with params built by the CLI as a JSON literal.
async p=>{const all=await indexedDB.databases(),req=q=>new Promise((r,j)=>{q.onsuccess=()=>r(q.result);q.onerror=()=>j(q.error)}),
open=k=>{const d=all.find(x=>x.name.includes(':'+k+':'));if(!d)throw Error('layout');return req(indexedDB.open(d.name))},
iso=t=>t?new Date(+t).toISOString():null,uid=m=>(m||'').replace(/^.*\//,'').replace(/^8:orgid:/,''),sys=t=>/^(ThreadActivity|Event|Control)\//.test(t||''),
page=rows=>{rows.sort((a,b)=>b.t-a.t||(a.id<b.id?-1:a.id>b.id?1:0));const c=p.after;if(c)rows=rows.filter(r=>r.t<c.t||r.t==c.t&&r.id>c.id);
const items=rows.slice(0,p.limit),last=items[items.length-1];return{items,next:rows.length>p.limit?{t:last.t,id:last.id}:null}},
prof=async ids=>{const n={};let d;try{d=await open('profiles');const s=d.transaction('profiles').objectStore('profiles');
await Promise.all(ids.map(async i=>{const r=await req(s.get('8:orgid:'+i));if(r&&r.displayName)n[i]=r.displayName}))}catch(e){}finally{d&&d.close()}return n},
chain=async(d,id)=>(await req(d.transaction('replychains').objectStore('replychains').getAll(IDBKeyRange.bound([id],[id,[]])))).flatMap(rc=>Object.values(rc.messageMap||{}));
const cdb=await open('conversation-manager'),me=cdb.name.split(':')[4],cs=cdb.transaction('conversations').objectStore('conversations');let pg;
try{if(p.query=='chats')pg=page((await req(cs.getAll())).filter(c=>c.type=='Chat'||c.type=='Meeting'||c.type=='StreamOfNotes').map(c=>({t:c.lastMessageTimeUtc||0,id:c.id,c})));
else if(!await req(cs.get(p.chat)))return{error:'not_found'}}finally{cdb.close()}
const rdb=await open('replychain-manager');try{if(pg){
const cs=pg.items.map(({c})=>{const lm=c.lastMessage||{},one=/@unq\.gbl\.spaces$/.test(c.id),nm={},h=+((c.properties||{}).consumptionhorizon||'').split(';')[0],
mem=new Set((c.members||[]).map(m=>uid(m.id)).concat(one?c.id.slice(3).split('@')[0].split('_'):[]));if(lm.imdisplayname)nm[uid(lm.from)]=lm.imdisplayname;
return{c,one,mem,nm,v:{id:c.id,topic:(c.threadProperties||{}).topic||null,chatType:c.type=='Meeting'?'meeting':one||c.type=='StreamOfNotes'?'oneOnOne':'group',
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
