// Projects the Teams page's own IndexedDB cache; raw records never leave the page.
// Evaluated with JSON params and the fixed account-cache selector.
async (p, select) => {
  const cache = select(await indexedDB.databases());
  if (cache.error) return { error: cache.error };
  const req = q => new Promise((resolve, reject) => {
    q.onsuccess = () => resolve(q.result);
    q.onerror = () => reject(q.error);
  });
  const openName = name => {
    // If the cache disappeared after enumeration, abort rather than create a database.
    const q = indexedDB.open(name);
    q.onupgradeneeded = () => q.transaction.abort();
    return req(q);
  };
  const open = k => {
    if (!cache[k]) throw Error('layout');
    return openName(cache[k]);
  };
  // Missing cache timestamps can be -1. Keep sort keys valid for CLI cursors.
  const time = t => Number.isFinite(+t) && +t > 0 && +t <= 8640000000000000 ? Math.trunc(+t) : 0;
  const iso = t => time(t) ? new Date(time(t)).toISOString() : null;
  // Work identities match Graph's object IDs. Personal/Skype MRIs keep their namespace.
  const uid = m => typeof m === 'string' ? m.replace(/^.*\//, '').replace(/^8:orgid:/, '') : '';
  const system = t => /^(ThreadActivity|Event|Control)\//.test(t || '');
  const page = rows => {
    rows.sort((a, b) => b.t - a.t || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    const c = p.after;
    if (c) rows = rows.filter(r => r.t < c.t || r.t == c.t && r.id > c.id);
    const items = rows.slice(0, p.limit), last = items[items.length - 1];
    return { items, next: rows.length > p.limit ? { t: last.t, id: last.id } : null };
  };
  const profiles = async ids => {
    const names = new Map();
    // The personal cache account key is an object ID, not the user's messaging MRI.
    let me = p.personal ? null : cache.account, db;
    try {
      db = await open('profiles');
      const store = db.transaction('profiles').objectStore('profiles');
      const own = new Promise((resolve, reject) => {
        let found = null;
        const q = store.openCursor();
        q.onerror = () => reject(q.error);
        q.onsuccess = () => {
          const cursor = q.result;
          if (!cursor) return resolve(found === null ? me : found);
          const record = cursor.value;
          const id = record && uid(record.mri);
          if (id && record.objectId === cache.account) {
            // Conflicting aliases must not arbitrarily identify the viewer.
            if (found !== null && found !== id) return resolve(null);
            found = id;
          }
          cursor.continue();
        };
      });
      // Queue lookups before awaiting so the readonly transaction remains active.
      const lookups = ids.map(async id => {
        const record = await req(store.get(id.startsWith('8:') ? id : '8:orgid:' + id));
        if (record && typeof record.displayName === 'string' && record.displayName) names.set(id, record.displayName);
      });
      [me] = await Promise.all([own, Promise.all(lookups)]);
    } catch (_) {
      // Profiles are optional. Missing names or viewer identity must not hide chats.
    } finally {
      if (db) db.close();
    }
    return { names, me };
  };
  // Teams can split reply chains across several stores (replychains, replychains-2, ...).
  // Numeric order keeps the primary store's copy of a message found in more than one.
  const chain = async (db, id) => {
    const rank = n => +(n.split('-')[1] || 1);
    const names = [...db.objectStoreNames].filter(n => /^replychains(-\d+)?$/.test(n)).sort((a, b) => rank(a) - rank(b));
    if (!names.length) throw Error('layout');
    const tx = db.transaction(names);
    const lists = await Promise.all(names.map(n => req(tx.objectStore(n).getAll(IDBKeyRange.bound([id], [id, []])))));
    return lists.flat().flatMap(rc => Object.values((rc && rc.messageMap) || {}));
  };

  const cdb = await open('conversation');
  let pg;
  try {
    const store = cdb.transaction('conversations').objectStore('conversations');
    if (p.query == 'chats') {
      pg = page((await req(store.getAll()))
        .filter(c => c.type == 'Chat' || c.type == 'Meeting' || c.type == 'StreamOfNotes')
        .map(c => ({ t: time(c.lastMessageTimeUtc), id: c.id, c })));
    } else if (!await req(store.get(p.chat))) return { error: 'not_found' };
  } finally { cdb.close(); }
  const rdb = await open('replies');
  try {
    if (pg) {
      const chats = pg.items.map(({ c }) => {
        const lm = c.lastMessage || {}, one = /@unq\.gbl\.spaces$/.test(c.id), names = new Map();
        const horizon = +((c.properties || {}).consumptionhorizon || '').split(';')[0];
        const members = new Set((c.members || []).map(m => uid(m.id))
          .concat(one ? c.id.slice(3).split('@')[0].split('_') : []));
        if (lm.imdisplayname) names.set(uid(lm.from), lm.imdisplayname);
        return { c, one, members, names, value: {
          id: c.id, topic: (c.threadProperties || {}).topic || null,
          chatType: c.type == 'Meeting' ? 'meeting' : one || c.type == 'StreamOfNotes' ? 'oneOnOne' : 'group',
          lastUpdatedDateTime: iso(c.lastMessageTimeUtc),
          viewpoint: { lastMessageReadDateTime: horizon ? iso(horizon) : null },
          lastMessagePreview: lm.id ? {
            id: lm.id, createdDateTime: iso(Date.parse(lm.originalarrivaltime) || lm.id),
            isDeleted: !!lm.deletionInfo, messageType: system(lm.messagetype) ? 'systemEventMessage' : 'message',
            from: { user: { id: uid(lm.from), displayName: lm.imdisplayname || null } },
            body: { contentType: /^RichText/.test(lm.messagetype) ? 'html' : 'text', content: lm.content || '' },
          } : null,
        } };
      });
      const { names, me } = await profiles([...new Set(chats.flatMap(x => [
        ...x.members, x.value.lastMessagePreview && x.value.lastMessagePreview.from.user.id,
      ]))].filter(Boolean));
      for (const x of chats) {
        const unknown = () => [...x.members].filter(id => id != me && !names.has(id) && !x.names.has(id));
        if (x.one && (unknown().length || x.members.size < 2)) {
          for (const m of await chain(rdb, x.c.id)) {
            const id = uid(m.creator);
            if (id && id != me && m.imDisplayName) {
              x.members.add(id);
              if (!x.names.has(id)) x.names.set(id, m.imDisplayName);
            }
            if (!unknown().length && x.members.size > 1) break;
          }
        }
        x.value.members = [...x.members].filter(Boolean).map(id => ({
          userId: id, displayName: names.get(id) || x.names.get(id) || null,
        }));
        if (x.value.lastMessagePreview) {
          const user = x.value.lastMessagePreview.from.user;
          user.displayName = user.displayName || names.get(user.id) || null;
        }
      }
      return { me, items: chats.map(x => x.value), next: pg.next };
    }
    // The current cache wins; older caches of the same account only add messages it lacks.
    const messages = await chain(rdb, p.chat);
    for (const name of cache.older || []) {
      let db;
      try {
        db = await openName(name);
        messages.push(...await chain(db, p.chat));
      } catch (_) {
        // Older caches are optional; one that vanished or changed layout is skipped.
      } finally {
        if (db) db.close();
      }
    }
    const rows = [], seen = new Set();
    for (const m of messages) {
      if (seen.has(m.id)) continue;
      seen.add(m.id);
      if (system(m.messageType)) continue;
      rows.push({ t: time(m.originalArrivalTime), id: m.id, value: {
        id: m.id, createdDateTime: iso(m.originalArrivalTime),
        clientMessageId: typeof m.clientMessageId === 'string' ? m.clientMessageId : null,
        status: typeof m.status === 'string' ? m.status : null,
        from: { user: { id: uid(m.creator), displayName: m.imDisplayName || null } },
        body: { contentType: /^RichText/.test(m.messageType) ? 'html' : 'text', content: m.content || '' },
        deleted: !!m.deletionInfo,
      } });
    }
    pg = page(rows);
    const { names, me } = await profiles([...new Set(pg.items.map(r => r.value.from.user)
      .filter(u => !u.displayName && u.id).map(u => u.id))]);
    for (const r of pg.items) {
      const user = r.value.from.user;
      user.displayName = user.displayName || names.get(user.id) || null;
    }
    return { me, items: pg.items.map(r => r.value), next: pg.next };
  } finally { rdb.close(); }
}
