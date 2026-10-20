// Select one account's cache databases. Several account caches can coexist in a
// storage context; their records must never be joined or sent through arbitrarily.
all => {
  const candidates = kind => all.filter(db =>
    typeof db.name === 'string' && db.name.includes(':' + kind + ':'));
  // Teams names caches `<prefix>:<kind>:<client>:<tenant>:<user>:<locale>`. Other
  // accounts, guest organizations and earlier display languages leave their own; the
  // signed-in one is what Teams itself records, and anything less agreed is refused.
  const signedIn = () => {
    try {
      const item = key => JSON.parse(localStorage.getItem(key) ?? 'null');
      const user = item('tmp.auth.v1.GLOBAL.PrimaryUserId.PrimaryUserId')?.item;
      const primary = item('tmp.react-web-client.cachedPrimaryUser');
      if (typeof user !== 'string' || primary?.userId !== user || typeof primary.tenantId !== 'string') return null;
      const locale = localStorage.getItem(`tmp.${primary.tenantId}.${user}.react-web-client.applicationLocaleCode`);
      return { tenant: primary.tenantId, user, locale };
    } catch {
      return null;
    }
  };
  let conversations = candidates('conversation-manager');
  if (conversations.length > 1) {
    const active = signedIn();
    const part = (db, index) => db.name.split(':')[index];
    if (active) conversations = conversations.filter(db =>
      part(db, 3) === active.tenant && part(db, 4) === active.user);
    if (active && conversations.length > 1) conversations = conversations.filter(db => part(db, 5) === active.locale);
    if (conversations.length !== 1) return { error: 'cache_ambiguous' };
  }
  if (conversations.length !== 1) return { error: 'layout' };
  const conversation = conversations[0].name;
  const account = conversation.split(':')[4];
  if (!account) return { error: 'layout' };
  const related = kind => {
    const sibling = conversation.replace(':conversation-manager:', ':' + kind + ':');
    if (all.some(db => db.name === sibling)) return sibling;
    const matches = candidates(kind).filter(db => db.name.split(':')[4] === account);
    return matches.length === 1 ? matches[0].name : null;
  };
  return { account, conversation, replies: related('replychain-manager'), profiles: related('profiles') };
}
