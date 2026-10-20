// Resolve people and open a one-on-one using Teams' own signed-in GraphQL client.
// No message is sent here. Parameters are JSON, never interpolated code.
async (p, select) => {
  const fiber = (e) => {
    const k = e && Object.keys(e).find((k) => k.startsWith("__reactFiber$"));
    return k && e[k];
  };
  const client =
    window.__teamsCliCore?.ready === true
      ? window.__teamsCliCore.client
      : (() => {
          for (const s of [
            '[contenteditable="true"]',
            "#app [data-tid]",
            "#app *",
          ]) {
            let n = 0;
            for (
              let f = fiber(document.querySelector(s));
              f && n++ < 5000;
              f = f.return
            ) {
              const v = f.type?._context && f.memoizedProps?.value;
              if (v?.defaultClient?.query && v.defaultClient.mutate)
                return v.defaultClient;
            }
          }
        })();
  if (!client) return { error: "layout" };
  const cache = select(await indexedDB.databases());
  if (cache.error) return { error: cache.error };
  const N = (value) => ({ kind: "Name", value }),
    V = (value) => ({ kind: "Variable", name: N(value) }),
    F = (name, children, args) => ({
      kind: "Field",
      name: N(name),
      arguments: (args || []).map((n) => ({
        kind: "Argument",
        name: N(n),
        value: V(n),
      })),
      directives: [],
      ...(children
        ? {
            selectionSet: {
              kind: "SelectionSet",
              selections: children.map((n) =>
                typeof n === "string" ? F(n) : n,
              ),
            },
          }
        : {}),
    });
  const type = (t) =>
    t.endsWith("!")
      ? { kind: "NonNullType", type: type(t.slice(0, -1)) }
      : t.startsWith("[")
        ? { kind: "ListType", type: type(t.slice(1, -1)) }
        : { kind: "NamedType", name: N(t) };
  const operationDoc = (operation, name, vars, fields) => ({
    kind: "Document",
    definitions: [
      {
        kind: "OperationDefinition",
        operation,
        name: N(name),
        directives: [],
        variableDefinitions: vars.map(([n, t]) => ({
          kind: "VariableDefinition",
          variable: V(n),
          type: type(t),
          directives: [],
        })),
        selectionSet: { kind: "SelectionSet", selections: fields },
      },
    ],
  });
  if (p.op === "search") {
    const query = operationDoc(
      "query",
      "TeamsCliSearchPeople",
      [
        ["searchQuery", "String"],
        ["peoplePickerFilters", "PeoplePickerFiltersInput"],
      ],
      [
        F(
          "searchUsers",
          [
            "id",
            "displayName",
            "email",
            "mri",
            "userPrincipalName",
            "type",
            "userType",
          ],
          ["searchQuery", "peoplePickerFilters"],
        ),
      ],
    );
    const filters = {
      name: "newChat",
      includeAADTenant: true,
      includeGuest: true,
      includeFederated: true,
      includeFederatedConsumer: true,
      includePersonalContact: true,
      includeSkypeConsumer: true,
      includeTflConsumer: true,
      includeXtap: true,
      includeBot: false,
      includeDL: false,
      includePstn: false,
      includeOffNetworkEmails: false,
      includeOffNetworkPhones: false,
    };
    try {
      const fields = [
        "id",
        "displayName",
        "email",
        "mri",
        "userPrincipalName",
        "type",
        "userType",
      ];
      let users;
      if (p.personal) {
        const cached = operationDoc(
          "query",
          "TeamsCliSearchContacts",
          [
            ["searchQuery", "String"],
            ["peoplePickerFilters", "PeoplePickerFiltersInput"],
          ],
          [
            F(
              "getCacheResults",
              [
                F("peopleResults", fields),
                F("rawContactsResults", fields),
                F("mfuContactsResults", fields),
              ],
              ["searchQuery", "peoplePickerFilters"],
            ),
          ],
        );
        const r = await client.query({
          query: cached,
          variables: { searchQuery: p.query, peoplePickerFilters: filters },
          fetchPolicy: "network-only",
        });
        const c = r.data?.getCacheResults;
        if (!c) return { error: "layout" };
        // Teams' MFU cache includes suggestions even for an exact email query.
        // Keep only matching cached people so suggestions cannot crowd out
        // the authoritative live lookup below.
        const terms = p.query.trim().toLowerCase().split(/\s+/);
        users = [
          ...(c.peopleResults || []),
          ...(c.rawContactsResults || []),
          ...(c.mfuContactsResults || []),
        ].filter((u) => {
          const text = [u.displayName, u.email, u.userPrincipalName]
            .filter((value) => typeof value === "string")
            .join(" ")
            .toLowerCase();
          return terms.every((term) => text.includes(term));
        });
        {
          const exact = operationDoc(
            "query",
            "TeamsCliResolveEmail",
            [
              ["emails", "[String!]!"],
              ["phones", "[String!]!"],
              ["callerInfo", "String"],
              ["shouldFilterBlockedContacts", "Boolean"],
              ["searchQuery", "String"],
            ],
            [
              F("instantScdSearch", fields, [
                "emails",
                "phones",
                "callerInfo",
                "shouldFilterBlockedContacts",
                "searchQuery",
              ]),
            ],
          );
          const found = await client.query({
            query: exact,
            variables: {
              emails: /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(p.query)
                ? [p.query]
                : [],
              phones: [],
              callerInfo: "newChat",
              shouldFilterBlockedContacts: true,
              // A nonempty searchQuery overrides emails in Teams' resolver.
              // Exact email lookup and name search need distinct call shapes.
              ...(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(p.query)
                ? {}
                : { searchQuery: p.query }),
            },
            fetchPolicy: "no-cache",
          });
          if (!Array.isArray(found.data?.instantScdSearch))
            return { error: "layout" };
          users.unshift(...found.data.instantScdSearch);
        }
      } else {
        const r = await client.query({
          query,
          variables: { searchQuery: p.query, peoplePickerFilters: filters },
          fetchPolicy: "network-only",
        });
        users = r.data?.searchUsers;
        if (!Array.isArray(users)) return { error: "layout" };
      }
      const seen = new Set(),
        items = [];
      for (const u of users) {
        const id = u.mri || u.id;
        if (
          typeof id !== "string" ||
          !/^8:[^\s<>"']{1,250}$/.test(id) ||
          seen.has(id)
        )
          continue;
        seen.add(id);
        items.push({
          id,
          displayName: u.displayName || u.email || u.userPrincipalName || id,
          ...(u.email || u.userPrincipalName
            ? { email: u.email || u.userPrincipalName }
            : {}),
          source: "desktop",
        });
      }
      return {
        items: items.slice(0, p.limit),
        truncated: items.length > p.limit,
      };
    } catch {
      return { error: "search_failed" };
    }
  }
  if (p.op !== "create" || !/^8:[^\s<>"']{1,250}$/.test(p.recipient))
    return { error: "invalid_input" };
  const req = (q) =>
    new Promise((r, j) => {
      q.onsuccess = () => r(q.result);
      q.onerror = () => j(q.error);
    });
  const open = (name) => {
    const q = indexedDB.open(name);
    q.onupgradeneeded = () => q.transaction.abort();
    return req(q);
  };
  let db,
    own = [];
  try {
    if (!cache.profiles) return { error: "layout" };
    db = await open(cache.profiles);
    const profiles = await req(
      db.transaction("profiles").objectStore("profiles").getAll(),
    );
    own = [
      ...new Set(
        profiles
          .filter(
            (u) => u.objectId === cache.account && typeof u.mri === "string",
          )
          .map((u) => u.mri.replace(/^.*\//, "")),
      ),
    ];
  } catch {
    return { error: "layout" };
  } finally {
    db?.close();
  }
  if (own.length !== 1) return { error: own.length ? "cache_ambiguous" : "layout" };
  if (own[0] === p.recipient) return {id:"48:notes",chatType:"oneOnOne",members:[{userId:own[0]}],source:"desktop"};
  const userIds = [...new Set([own[0], p.recipient])];
  const lookup = operationDoc(
    "query",
    "TeamsCliExistingChat",
    [
      ["userIds", "[ID!]!"],
      ["createLieIfNotFound", "Boolean"],
      ["onlyFetchNonLieConversation", "Boolean"],
    ],
    [
      F(
        "chatsWithUsers",
        ["id", "title", "memberIds", "chatType"],
        ["userIds", "createLieIfNotFound", "onlyFetchNonLieConversation"],
      ),
    ],
  );
  const project = (c) => ({
    id: c.id,
    topic: c.title || null,
    chatType: "oneOnOne",
    members: (c.memberIds || userIds).map((id) => ({ userId: id })),
    source: "desktop",
  });
  try {
    const r = await client.query({
      query: lookup,
      variables: {
        userIds,
        createLieIfNotFound: false,
        onlyFetchNonLieConversation: true,
      },
      fetchPolicy: "network-only",
    });
    const chats = r.data?.chatsWithUsers;
    if (!Array.isArray(chats)) return { error: "layout" };
    const c = chats.find(
      (c) =>
        /^19:[^\s]+$/.test(c.id) &&
        Array.isArray(c.memberIds) &&
        c.memberIds.length === userIds.length &&
        userIds.every((id) => c.memberIds.includes(id)),
    );
    if (c) return project(c);
  } catch {
    return { error: "search_failed" };
  }
  const mutation = operationDoc(
    "mutation",
    "TeamsCliCreateChat",
    [["userIds", "[ID!]!"]],
    [
      F(
        "createChat",
        [
          F("conversation", ["id", "title", "memberIds", "chatType"]),
          "hasIBPolicyRejectedUsers",
          "hasChatPermissionsPolicyRejectedUsers",
        ],
        ["userIds"],
      ),
    ],
  );
  // Never retry a mutation: the chat may have been created even if its answer is lost.
  try {
    const r = await client.mutate({ mutation, variables: { userIds } }),
      v = r.data?.createChat;
    if (v?.hasIBPolicyRejectedUsers || v?.hasChatPermissionsPolicyRejectedUsers)
      return { error: "permission_denied" };
    const c = v?.conversation;
    return c && /^19:[^\s]+$/.test(c.id)
      ? project(c)
      : { error: "unconfirmed" };
  } catch {
    return { error: "unconfirmed" };
  }
}
