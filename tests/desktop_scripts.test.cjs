const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = file => '(' + fs.readFileSync(path.join(__dirname, '..', 'src', file), 'utf8') + ')';
const chat = '19:fixture@thread.v2';
const name = (kind, account) => `fixture:v1:${kind}:store:${account}`;
const conversation = { id: chat, type: 'Chat', lastMessageTimeUtc: 1700000000000,
  members: [{ id: '8:orgid:sender' }] };
const message = { id: '1700000000000', originalArrivalTime: 1700000000000,
  creator: '8:orgid:sender', messageType: 'Text', content: 'Use &amp; literally' };

function request(value) {
  const result = { result: value };
  queueMicrotask(() => result.onsuccess());
  return result;
}

function cursor(records) {
  const q = {};
  let index = 0;
  const next = () => queueMicrotask(() => {
    q.result = index < records.length ? { value: records[index++], continue: next } : null;
    q.onsuccess();
  });
  next();
  return q;
}

function fixture({ ambiguous = false, foreignOnly = false, mutate, timeout = false, core = false } = {}) {
  const databases = new Map([
    [name('conversation-manager', 'account-a'), [conversation]],
    [name('replychain-manager', 'account-b'), [{ chat, messageMap: { b: { ...message, content: 'Wrong account' } } }]],
    [name('profiles', 'account-b'), [{ mri: '8:orgid:sender', displayName: 'Wrong Name' }]],
  ]);
  if (!foreignOnly) {
    databases.set(name('replychain-manager', 'account-a'), [{ chat, messageMap: { a: message } }]);
    databases.set(name('profiles', 'account-a'), [{ mri: '8:orgid:sender', displayName: 'Correct Name' }]);
  }
  if (ambiguous) databases.set(name('conversation-manager', 'account-b'), [conversation]);
  const opened = [], mutations = [];
  let inspections = 0;
  const client = { mutate: options => {
    mutations.push(options);
    return mutate ? mutate(options) : Promise.resolve({ data: { sendMessage: { id: '1700000000001', status: 'Sent' } } });
  } };
  const element = { '__reactFiber$fixture': {
    type: { _context: {} }, memoizedProps: { value: { defaultClient: client } }, return: null,
  } };
  const context = vm.createContext({
    indexedDB: {
      databases: async () => { inspections++; return [...databases.keys()].map(name => ({ name })); },
      open: database => {
        assert.ok(databases.has(database), `Unknown database: ${database}`);
        opened.push(database);
        return request({ name: database, close() {}, transaction: () => ({ objectStore: () => ({
          get: id => request(databases.get(database).find(record => (record.mri || record.id) === id)),
          getAll: range => request(databases.get(database).filter(record => !range || record.chat === range.chat)),
          openCursor: () => cursor(databases.get(database)),
        }) }) });
      },
    },
    IDBKeyRange: { bound: a => ({ chat: a[0] }) },
    document: { querySelector: () => { assert.ok(!core, 'core send must not inspect React'); return element; } },
    crypto: { randomUUID: () => 'fixture-correlation-id' },
    setTimeout: callback => { if (timeout) queueMicrotask(callback); },
  });
  if (core) context.__teamsCliCore = { ready: true, client };
  const select = vm.runInContext(source('desktop_cache.js'), context);
  const read = vm.runInContext(source('desktop.js'), context);
  const send = vm.runInContext(source('desktop_send.js'), context);
  return { context, client, databases, opened, mutations, inspections: () => inspections,
    read: params => read({ query: 'messages', chat, limit: 50, after: null, personal: false, ...params }, select),
    send: params => send({ chat, content: 'fixture', clientMessageId: '123456789012345678', ...params }, select) };
}

test('messages and profile names come from the conversation account', async () => {
  const f = fixture();
  const result = await f.read();
  assert.equal(result.me, 'account-a');
  assert.equal(result.items.length, 1);
  assert.equal(result.items[0].body.content, message.content);
  assert.equal(result.items[0].from.user.displayName, 'Correct Name');
  assert.ok(f.opened.every(database => database.endsWith(':account-a')));
});

function personalFixture() {
  const f = fixture();
  const me = '8:live:fixture-self', other = '8:fixture-recipient';
  f.databases.set(name('conversation-manager', 'account-a'), [
    { ...conversation, members: [{ id: me }, { id: other }], lastMessage: {
      id: message.id, from: me, messagetype: 'Text', content: 'Preview',
    } },
    { id: '48:notes', type: 'StreamOfNotes', members: [{ id: me }] },
  ]);
  f.databases.set(name('profiles', 'account-a'), [
    { mri: other, objectId: 'other-account', displayName: 'Recipient' },
    { mri: me, objectId: 'account-a', displayName: 'Self' },
  ]);
  f.databases.set(name('replychain-manager', 'account-a'), [{ chat, messageMap: {
    a: { ...message, creator: me },
    b: { ...message, id: '1700000000001', creator: other, messageType: 'RichText/Html', content: '<p>Reply</p>' },
  } }]);
  return { ...f, me, other };
}

test('personal and legacy Skype member names and viewer use exact MRIs', async () => {
  const f = personalFixture();
  const result = await f.read({ query: 'chats', personal: true });
  assert.equal(result.me, f.me);
  const conversation = result.items.find(c => c.id === chat);
  assert.equal(conversation.chatType, 'group'); // Preserve observed cache classification.
  assert.equal(conversation.lastMessagePreview.from.user.id, f.me);
  assert.equal(conversation.lastMessagePreview.from.user.displayName, 'Self');
  assert.deepEqual(Array.from(conversation.members, m => [m.userId, m.displayName]),
    [[f.me, 'Self'], [f.other, 'Recipient']]);
  assert.equal(result.items.find(c => c.id === '48:notes').members[0].userId, f.me);
  assert.ok(f.opened.every(database => database.endsWith(':account-a')));
});

test('personal messages retain sender identity, HTML, timestamps and stable pagination', async () => {
  const f = personalFixture();
  const first = await f.read({ personal: true, limit: 1 });
  assert.equal(first.me, f.me);
  assert.equal(first.items[0].from.user.id, f.me);
  assert.equal(first.items[0].from.user.displayName, 'Self');
  assert.equal(first.items[0].body.content, 'Use &amp; literally');
  assert.equal(first.items[0].createdDateTime, '2023-11-14T22:13:20.000Z');
  const second = await f.read({ personal: true, limit: 1, after: first.next });
  assert.equal(second.items[0].from.user.id, f.other);
  assert.equal(second.items[0].from.user.displayName, 'Recipient');
  assert.equal(second.items[0].body.contentType, 'html');
  assert.equal(second.items[0].body.content, '<p>Reply</p>');
  assert.equal(second.next, null);
});

test('personal identity namespaces do not collide with Skype or work identities', async () => {
  const f = personalFixture();
  const ids = ['8:live:same', '8:same', 'same'];
  f.databases.set(name('conversation-manager', 'account-a'), [{ ...conversation,
    members: ids.map(id => ({ id: id === 'same' ? '8:orgid:same' : id })) }]);
  f.databases.set(name('profiles', 'account-a'), ids.map((id, i) => ({
    mri: id === 'same' ? '8:orgid:same' : id, displayName: `Name ${i}`,
  })));
  const result = await f.read({ query: 'chats', personal: true });
  assert.deepEqual(Array.from(result.items[0].members, m => [m.userId, m.displayName]),
    ids.map((id, i) => [id, `Name ${i}`]));
  assert.equal(result.me, null);
});

test('missing personal profiles leave the viewer unknown and reads available', async () => {
  const f = personalFixture();
  f.databases.delete(name('profiles', 'account-a'));
  const result = await f.read({ personal: true });
  assert.equal(result.me, null);
  assert.equal(result.items.length, 2);
  assert.equal(result.items[0].from.user.id, f.me);
  assert.equal(result.items[0].from.user.displayName, null);
  assert.ok(f.opened.every(database => database.endsWith(':account-a')));
});

test('conflicting viewer mappings never pick an arbitrary cached profile', async () => {
  const f = personalFixture();
  f.databases.get(name('profiles', 'account-a')).push({
    mri: '8:live:conflicting-self', objectId: 'account-a', displayName: 'Conflicting Self',
  });
  for (const personal of [false, true]) {
    const result = await f.read({ personal });
    assert.equal(result.me, null);
    assert.equal(result.items.length, 2);
  }
});

test('work viewer mapping keeps Graph object IDs and excludes foreign own profiles', async () => {
  const f = fixture();
  f.databases.get(name('profiles', 'account-b')).push({
    mri: '8:live:foreign-self', objectId: 'account-a', displayName: 'Foreign Self',
  });
  assert.equal((await f.read()).me, 'account-a');
  f.databases.get(name('profiles', 'account-a')).push({
    mri: '8:orgid:account-a', objectId: 'account-a', displayName: 'Self',
  });
  assert.equal((await f.read()).me, 'account-a');
  assert.equal((await f.read({ personal: true })).me, 'account-a');
  assert.ok(f.opened.every(database => database.endsWith(':account-a')));
});

test('chat members use the same account profile database', async () => {
  const f = fixture();
  const result = await f.read({ query: 'chats', chat: '' });
  assert.equal(result.items[0].members[0].displayName, 'Correct Name');
  assert.ok(f.opened.every(database => database.endsWith(':account-a')));
});

for (const query of ['chats', 'messages']) {
  test(`${query} paginate through missing and sentinel timestamps with valid cursors`, async () => {
    const f = fixture();
    const times = [-1, 0, undefined, 'invalid', Infinity, 8640000000000001, 1700000000000];
    const ids = times.map((_, i) => query === 'chats' ? `19:${i}@thread.v2` : String(i));
    if (query === 'chats') {
      f.databases.set(name('conversation-manager', 'account-a'), times.map((t, i) => ({
        ...conversation, id: ids[i], lastMessageTimeUtc: t,
      })));
    } else {
      f.databases.set(name('replychain-manager', 'account-a'), [{ chat, messageMap:
        Object.fromEntries(times.map((t, i) => [i, { ...message, id: ids[i], originalArrivalTime: t }])) }]);
    }
    let after = null;
    const seen = [];
    for (let page = 0; page < times.length; page++) {
      const result = await f.read({ query, limit: 1, after });
      assert.equal(result.items.length, 1);
      const item = result.items[0];
      seen.push(item.id);
      assert.equal(item[query === 'chats' ? 'lastUpdatedDateTime' : 'createdDateTime'],
        page === 0 ? '2023-11-14T22:13:20.000Z' : null);
      after = result.next;
      if (after) assert.ok(Number.isInteger(after.t) && after.t >= 0);
    }
    assert.equal(after, null);
    assert.deepEqual(seen, [ids.at(-1), ...ids.slice(0, -1)]);
    assert.equal(new Set(seen).size, times.length);
  });
}

test('a missing account database never falls back to another account', async () => {
  const f = fixture({ foreignOnly: true });
  await assert.rejects(f.read(), /layout/);
  assert.ok(f.opened.every(database => database.endsWith(':account-a')));
});

function selectWith(names, storage = {}) {
  const context = vm.createContext({ localStorage: { getItem: key => key in storage ? storage[key] : null } });
  // Compare plain values: objects from another context have their own prototypes.
  return JSON.parse(JSON.stringify(vm.runInContext(source('desktop_cache.js'), context)(names.map(name => ({ name })))));
}

const teams = (kind, tenant, user, locale = 'en-us') => `Teams:${kind}:react-web-client:${tenant}:${user}:${locale}`;
const signedIn = (tenant, user, locale) => ({
  'tmp.auth.v1.GLOBAL.PrimaryUserId.PrimaryUserId': JSON.stringify({ item: user }),
  'tmp.react-web-client.cachedPrimaryUser': JSON.stringify({ tenantId: tenant, userId: user }),
  ...(locale && { [`tmp.${tenant}.${user}.react-web-client.applicationLocaleCode`]: locale }),
});
const caches = (tenant, user, locale) => ['conversation-manager', 'replychain-manager', 'profiles']
  .map(kind => teams(kind, tenant, user, locale));

test('the signed-in user, tenant and display language select among coexisting caches', () => {
  const names = [
    ...caches('home', 'me', 'en-us'), ...caches('home', 'me', 'nl-nl'),
    ...caches('guest', 'me-as-guest'), ...caches('home', 'colleague'),
  ];
  for (const [tenant, user, locale] of [['home', 'me', 'nl-nl'], ['home', 'me', 'en-us'], ['guest', 'me-as-guest', 'en-us']]) {
    assert.deepEqual(selectWith(names, signedIn(tenant, user, locale)), {
      account: user, conversation: teams('conversation-manager', tenant, user, locale),
      replies: teams('replychain-manager', tenant, user, locale), profiles: teams('profiles', tenant, user, locale),
    });
  }
  // Only one cache for the signed-in account needs no display language.
  assert.equal(selectWith(names, signedIn('home', 'colleague')).conversation, teams('conversation-manager', 'home', 'colleague'));
});

test('caches are refused when Teams does not identify exactly one of them', () => {
  const names = [...caches('home', 'me', 'en-us'), ...caches('home', 'me', 'nl-nl'), ...caches('home', 'colleague')];
  const inconsistent = { ...signedIn('home', 'me', 'en-us'),
    'tmp.react-web-client.cachedPrimaryUser': JSON.stringify({ tenantId: 'home', userId: 'colleague' }) };
  for (const storage of [{}, signedIn('home', 'me'), signedIn('home', 'me', 'de-de'), signedIn('home', 'stranger', 'en-us'), inconsistent,
    { 'tmp.auth.v1.GLOBAL.PrimaryUserId.PrimaryUserId': 'not json' }]) {
    assert.deepEqual(selectWith(names, storage), { error: 'cache_ambiguous' });
  }
});

test('ambiguous account caches are refused before reading or sending', async () => {
  const f = fixture({ ambiguous: true });
  assert.equal((await f.read()).error, 'cache_ambiguous');
  assert.equal((await f.send()).error, 'cache_ambiguous');
  assert.deepEqual(f.opened, []);
  assert.deepEqual(f.mutations, []);
});

test('send check inspects no cache and performs no mutation', async () => {
  const f = fixture({ ambiguous: true });
  assert.equal((await f.send({ probe: true })).main, true);
  assert.equal(f.inspections(), 0);
  assert.deepEqual(f.opened, []);
  assert.deepEqual(f.mutations, []);
});

test('a confirmed send runs once and preserves literal plain text by escaping HTML', async () => {
  const f = fixture();
  const result = await f.send({ content: 'Use &amp; literally\n<tag>' });
  assert.equal(result.id, '1700000000001');
  assert.equal(f.mutations.length, 1);
  assert.equal(f.mutations[0].variables.convId, chat);
  assert.equal(f.mutations[0].variables.message.content, '<p>Use &amp;amp; literally<br>&lt;tag&gt;</p>');
  assert.deepEqual(f.opened, [name('conversation-manager', 'account-a')]);
});

test('URLs are sent as Teams links, leaving trailing punctuation and other text escaped', async () => {
  const f = fixture();
  const link = (url) =>
    `<a href="${url}" itemtype="http://schema.skype.com/HyperLink" rel="noreferrer noopener" title="${url}" target="_blank" itemid="fixture-correlation-id">${url}</a>`;
  await f.send({
    content: 'MR: https://x.test/a?b=1&c=2.\n(see https://x.test/wiki/A_(b)) <https://x.test/z> https://.',
  });
  assert.equal(
    f.mutations[0].variables.message.content,
    `<p>MR: ${link('https://x.test/a?b=1&amp;c=2')}.<br>(see ${link('https://x.test/wiki/A_(b)')}) &lt;${link('https://x.test/z')}&gt; https://.</p>`,
  );
});

test('missing chats never reach the send client', async () => {
  const f = fixture();
  assert.equal((await f.send({ chat: '19:absent@thread.v2' })).error, 'not_found');
  assert.deepEqual(f.mutations, []);
});

test('a rejected or timed-out mutation is unconfirmed and never retried', async () => {
  for (const options of [
    { mutate: () => Promise.reject(new Error('fixture failure')) },
    { mutate: () => new Promise(() => {}), timeout: true },
  ]) {
    const f = fixture(options);
    assert.equal((await f.send()).error, 'unconfirmed');
    assert.equal(f.mutations.length, 1);
  }
});


test('message projection preserves exact send correlation and raw delivery status', async () => {
  const f = fixture();
  f.databases.set(name('replychain-manager', 'account-a'), [{ chat, messageMap: {
    a: { ...message, clientMessageId: '100000000000000001', status: 'Queued' },
  } }]);
  const result = await f.read();
  assert.equal(result.items[0].clientMessageId, '100000000000000001');
  assert.equal(result.items[0].status, 'Queued');
});

// The UI-free runtime must use its retained client, with the cache-miss fallback,
// while still issuing exactly one mutation and returning only send metadata.
test('core sends use the retained client and enable chat network fallback once', async () => {
  const f = fixture({ core: true });
  const result = await f.send();
  assert.equal(result.id, '1700000000001');
  assert.equal(f.mutations.length, 1);
  const mutation = f.mutations[0];
  assert.equal(mutation.variables.withChatNetworkFallback, true);
  const operation = mutation.mutation.definitions[0];
  assert.ok(operation.variableDefinitions.some(v => v.variable.name.value === 'withChatNetworkFallback'));
  assert.ok(operation.selectionSet.selections[0].arguments.some(v => v.name.value === 'withChatNetworkFallback'));
});


test('core readiness requires client setup, send dependencies and a fresh query', async () => {
  const f = personalFixture();
  f.context.window = f.context;
  let setupCalls = 0, queryCalls = 0;
  const services = ['getAsyncMediaService','getImageProxyService','getUrlPreviewService','getFilesReferenceService','getAppBasedLinkService','getBotsPlatformService'];
  const initialized = [];
  f.client.getContext = () => Object.fromEntries(services.map(name => [name, async () => { initialized.push(name); return {}; }]));
  f.client.query = async options => {
    queryCalls++;
    assert.equal(options.fetchPolicy, 'network-only');
    assert.equal(options.variables.convId, '48:notes');
    return { data: { chat: { id: '48:notes' } } };
  };
  const bootstrap = vm.runInContext(source('desktop_core.js'), f.context);
  const context = { client: f.client };
  bootstrap({ _renderingContextManager: { primaryUserContext: { context } } }, id => {
    assert.equal(id, 630818); return { A: {} };
  }, (client, value) => { assert.equal(client, f.client); assert.equal(value, context); setupCalls++; });
  const state = f.context.__teamsCliCore;
  assert.equal(state.ready, false);
  assert.equal(await state.prepare(), true);
  assert.equal(state.ready, true);
  assert.equal(setupCalls, 1);
  assert.equal(queryCalls, 1);
  assert.deepEqual(initialized.sort(), services.sort());
  assert.equal(f.mutations.length, 0);
});

test('persisted chat caches never establish core readiness after a query failure', async () => {
  const f = personalFixture(); f.context.window = f.context;
  f.client.getContext = () => new Proxy({}, { get: () => async () => ({}) });
  f.client.query = async () => { throw new Error('fixture authentication failed'); };
  vm.runInContext(source('desktop_core.js'), f.context)(
    { _renderingContextManager: { primaryUserContext: { context: { client: f.client } } } },
    () => ({ A: {} }), () => {});
  assert.equal(await f.context.__teamsCliCore.prepare(), false);
  assert.equal(f.context.__teamsCliCore.ready, false);
  assert.equal(f.mutations.length, 0);
});
