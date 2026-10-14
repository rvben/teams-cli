// Called inside the pinned Teams bootstrap module, before its UI renderer.
// Keep the authenticated data runtime; no UI roots or alternate transport.
(entry,webpack,setup)=>{
const state={entry,webpack,client:null,ready:false,pending:null};
Object.defineProperty(window,'__teamsCliCore',{value:state,configurable:false});
state.prepare=async()=>{
 if(state.pending)return state.pending;
 state.pending=(async()=>{
  const context=entry._renderingContextManager?.primaryUserContext?.context;
  if(!context?.client) return false;
  setup(context.client,context);state.client=context.client;
  const ctx=state.client.getContext();
  await Promise.all(['getAsyncMediaService','getImageProxyService','getUrlPreviewService','getFilesReferenceService','getAppBasedLinkService','getBotsPlatformService'].map(k=>ctx[k]()));
  // A persisted cache alone does not establish a working authenticated client.
  const databases=await indexedDB.databases(),selected=databases.filter(d=>d.name?.includes(':conversation-manager:'));
  if(selected.length!==1)return false;
  const req=q=>new Promise((r,j)=>{q.onsuccess=()=>r(q.result);q.onerror=()=>j(q.error)}),db=await req(indexedDB.open(selected[0].name));
  let rows;try{rows=await req(db.transaction('conversations').objectStore('conversations').getAll())}finally{db.close()}
  const self=rows.filter(c=>c.type==='StreamOfNotes');if(self.length!==1)return false;
  const result=await state.client.query({query:webpack(630818).A,variables:{convId:self[0].id},fetchPolicy:'network-only'});
  state.ready=!!result.data?.chat;return state.ready;
 })().catch(()=>false).finally(()=>{state.pending=null});
 return state.pending;
};
}
