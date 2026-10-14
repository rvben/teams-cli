// Sends one plain-text chat message, with URLs as links, through the Teams window's own
// GraphQL client, exactly as its Send button does; Teams performs the request with its own
// sign-in. Evaluated as `(script)(params, select)`, with JSON params and the fixed cache
// selector. Returns only ids, timestamps and Teams' status label, never message contents.
// `{probe: true}` only reports whether this page hosts the client and sends nothing.
async(p,select)=>{const fiber=e=>{const k=e&&Object.keys(e).find(k=>k.startsWith('__reactFiber$'));return k&&e[k]},
core=globalThis.__teamsCliCore?.ready===true?globalThis.__teamsCliCore:null,
client=core?.client||(()=>{for(const s of['[contenteditable="true"]','#app [data-tid]','#app *']){let n=0;
for(let f=fiber(document.querySelector(s));f&&n<5000;f=f.return,n++){const v=f.type&&f.type._context&&f.memoizedProps&&f.memoizedProps.value;
if(v&&v.defaultClient&&typeof v.defaultClient.mutate=='function')return v.defaultClient}}})();
if(p.probe)return{main:!!client};if(!client)return{error:'layout'};
const req=q=>new Promise((r,j)=>{q.onsuccess=()=>r(q.result);q.onerror=()=>j(q.error)}),
cache=select(await indexedDB.databases());if(cache.error)return{error:cache.error};
const db=await req(indexedDB.open(cache.conversation));try{if(!await req(db.transaction('conversations').objectStore('conversations').get(p.chat)))return{error:'not_found'}}finally{db.close()}
const N=v=>({kind:'Name',value:v}),V=v=>({kind:'Variable',name:N(v)}),T=(t,r)=>r?{kind:'NonNullType',type:{kind:'NamedType',name:N(t)}}:{kind:'NamedType',name:N(t)},
vars=[['convId','ID',1],['message','SendMessageInput',1],['action','SendMessageAction',1],['originalConvId','ID'],['correlationId','ID'],
['offlineActionOptions','OfflineActionOptions'],['includeRepliesSummary','Boolean',0,{kind:'BooleanValue',value:false}]],
field=v=>({kind:'Field',name:N(v)}),
coreVar=['withChatNetworkFallback','Boolean'],
sendVars=core?vars.slice(0,6).concat([coreVar]):vars.slice(0,6),
allVars=core?vars.concat([coreVar]):vars,
mutation={kind:'Document',definitions:[{kind:'OperationDefinition',operation:'mutation',name:N('autoSendMessage'),
variableDefinitions:allVars.map(([n,t,r,dv])=>({kind:'VariableDefinition',variable:V(n),type:T(t,r),defaultValue:dv,directives:[]})),
directives:[{kind:'Directive',name:N('offlineAction'),arguments:[]},{kind:'Directive',name:N('cache'),arguments:[]}],
selectionSet:{kind:'SelectionSet',selections:[{...field('sendMessage'),arguments:sendVars.map(([n])=>({kind:'Argument',name:N(n),value:V(n)})),
directives:[],selectionSet:{kind:'SelectionSet',selections:['id','clientMessageId','originalArrivalTime','status','messageType'].map(field)}}]}}]},
esc=s=>s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'),
text=s=>esc(s).replace(/\r?\n/g,'<br>'),
// A URL becomes a link the way Teams' compose box makes one; trailing punctuation and an
// unbalanced `)` stay outside it, so "see https://x/a." links https://x/a.
link=u=>{const h=esc(u);return'<a href="'+h+'" itemtype="http://schema.skype.com/HyperLink" rel="noreferrer noopener" title="'+h+'" target="_blank" itemid="'+crypto.randomUUID()+'">'+h+'</a>'},
html=s=>s.split(/(https?:\/\/[^\s<>"]+)/i).map((x,i)=>{if(i%2==0)return text(x);let u=x.replace(/[.,;:!?']+$/,'');
while(u.endsWith(')')&&u.split('(').length<u.split(')').length)u=u.slice(0,-1).replace(/[.,;:!?']+$/,'');
return u.includes('://')&&!u.endsWith('//')?link(u)+text(x.slice(u.length)):text(x)}).join(''),
now=String(Date.now()),
message={replyChainId:null,clientMessageId:p.clientMessageId,content:'<p>'+html(p.content)+'</p>',
computed:{amsReferences:[],cards:[],embeddedLinks:[],isExpanded:false,offlineAssets:[],mentions:[],onBehalfOf:[],urlPreview:null,appIdsToInstall:[],quotedMessages:[],blurHash:null},
files:[],importance:'Standard',messageType:'RichText/Html',originalArrivalTime:null,policyViolation:null,subject:'',deeplinks:[],meeting:null,
scheduledSendTime:null,targetedMsgRecipientId:null,forwardMessageIds:[],forwardMessageOriginalConvId:null,forwardedMessageContext:null,
forwardMessageRecipients:[],forwardAppIdsToInstall:[],forwardMessageCardContents:null,forwardPrivateReply:false,forwardTeamId:null,
isForwardedMessage:false,draftCreationTime:now,title:null,postType:null,replyPermission:null,fluidUrlPermissionInfo:null,fluidProperties:null,
crossPostChannels:[],crossPostId:null,sendPostAsEmail:null,shareToL1:null,sharedToMessageId:null,suggestedActions:null,from:null,hasEdit:false,
originalSendPipelineTime:now,overrideImDisplayNameT1:null,meetingChatType:null,sendPipelineThreadType:'Chats',isNewBotSession:false,
inMeetingPrivateChatContext:null},
variables={action:'Create',convId:p.chat,originalConvId:p.chat,correlationId:crypto.randomUUID(),searchSessionId:'',
sendFromChannel_ToBeRefactored:false,includeRepliesSummary:true,isBilateralChat:false,message,
offlineActionOptions:{queueId:'queue_'+p.chat,actionId:'send_'+p.clientMessageId,actionType:'send',pendingUploadIds:[]}};
if(core)variables.withChatNetworkFallback=true;
// From here on Teams may already have the message, so every failure is reported as unconfirmed.
try{const r=await Promise.race([client.mutate({mutation,variables}),new Promise(r=>setTimeout(r,20000))]),
m=r&&r.data&&r.data.sendMessage;if(!m||!m.id)return{error:'unconfirmed'};
return{id:String(m.id),clientMessageId:m.clientMessageId||p.clientMessageId,originalArrivalTime:m.originalArrivalTime||null,status:m.status||null}}
catch(e){return{error:'unconfirmed'}}}
