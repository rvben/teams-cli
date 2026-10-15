import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const source=fs.readFileSync(new URL('../../src/desktop_people.js',import.meta.url),'utf8');
function fixture({existing=false,failQuery=false,failMutation=false,policy=false,wrongMembers=false}={}){
 const calls=[];const client={query:async r=>{calls.push(['query',r]);if(failQuery)throw Error();const name=r.query.definitions[0].selectionSet.selections[0].name.value;return{data:{[name]: name==='chatsWithUsers'?(existing?[{id:'19:existing@thread.v2',memberIds:wrongMembers?['8:live:other','8:live:me']:['8:live:person','8:live:me']}]:[]):name==='getCacheResults'?{peopleResults:[{id:'8:live:person',displayName:'Person'}]}:[{id:'8:live:person',displayName:'Person'}]}}},mutate:async r=>{calls.push(['mutate',r]);if(failMutation)throw Error();return{data:{createChat:{conversation:{id:'19:new@thread.v2',memberIds:['8:live:me','8:live:person']},hasIBPolicyRejectedUsers:policy}}}}};
 const req=result=>{const q={result};queueMicrotask(()=>q.onsuccess?.());return q};
 const db={transaction:()=>({objectStore:()=>({getAll:()=>req([{objectId:'account',mri:'8:live:me'}])})}),close(){}};
 const run=vm.runInNewContext(`(${source})`,{window:{__teamsCliCore:{ready:true,client}},indexedDB:{databases:async()=>[],open:()=>req(db)}});
 return{calls,run:p=>run(p,()=>({account:'account',profiles:'profiles'}))};
}
test('personal name search uses Teams live lookup without mutation',async()=>{const f=fixture();const r=await f.run({op:'search',personal:true,query:'Person',limit:20});assert.equal(r.items.length,1);assert.equal(f.calls.filter(c=>c[0]==='mutate').length,0);assert.equal(f.calls[1][1].variables.searchQuery,'Person');assert.deepEqual(Array.from(f.calls[1][1].variables.emails),[])});
test('existing exact participant chat is reused without mutation',async()=>{const f=fixture({existing:true});const r=await f.run({op:'create',recipient:'8:live:person'});assert.equal(r.id,'19:existing@thread.v2');assert.equal(f.calls.length,1)});
test('new chat includes selected account and recipient and mutates once',async()=>{const f=fixture();const r=await f.run({op:'create',recipient:'8:live:person'});assert.equal(r.id,'19:new@thread.v2');assert.equal(f.calls.filter(c=>c[0]==='mutate').length,1);assert.deepEqual(Array.from(f.calls[1][1].variables.userIds),['8:live:me','8:live:person']);assert(!JSON.stringify(f.calls).includes('sendMessage'))});
test('a chat with different members cannot be reused',async()=>{const f=fixture({existing:true,wrongMembers:true});assert.equal((await f.run({op:'create',recipient:'8:live:person'})).id,'19:new@thread.v2')});
test('failed preflight never mutates; failed mutation never retries',async()=>{const pre=fixture({failQuery:true});assert.equal((await pre.run({op:'create',recipient:'8:live:person'})).error,'search_failed');assert.equal(pre.calls.length,1);const post=fixture({failMutation:true});assert.equal((await post.run({op:'create',recipient:'8:live:person'})).error,'unconfirmed');assert.equal(post.calls.filter(c=>c[0]==='mutate').length,1)});
test('Teams policy blocks are explicit',async()=>{const f=fixture({policy:true});assert.equal((await f.run({op:'create',recipient:'8:live:person'})).error,'permission_denied')});

test('self identity reuses notes without a query or mutation',async()=>{const f=fixture();const r=await f.run({op:'create',recipient:'8:live:me'});assert.equal(r.id,'48:notes');assert.equal(f.calls.length,0)});
