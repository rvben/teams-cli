// Select one account's cache databases. Several account caches can coexist in a
// storage context; their records must never be joined or sent through arbitrarily.
all => {
  const candidates = kind => all.filter(db =>
    typeof db.name === 'string' && db.name.includes(':' + kind + ':'));
  const conversations = candidates('conversation-manager');
  if (conversations.length > 1) return { error: 'account_ambiguous' };
  if (conversations.length !== 1) return { error: 'layout' };
  const conversation = conversations[0].name;
  const account = conversation.split(':')[4];
  if (!account) return { error: 'layout' };
  const related = kind => {
    const matches = candidates(kind).filter(db => db.name.split(':')[4] === account);
    return matches.length === 1 ? matches[0].name : null;
  };
  return { account, conversation, replies: related('replychain-manager'), profiles: related('profiles') };
}
