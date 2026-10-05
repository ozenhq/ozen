# Security

Sync promises that the relay never holds anything readable and that only your own Macs can read what they send each other. If you find a way to break that, or anything else in this repo, please tell us privately first.

## Reporting

- Use **Report a vulnerability** on this repo's [Security tab](https://github.com/ozenhq/ozen/security). It opens a private advisory that only the maintainers see.
- Don't open a public issue, PR or discussion about it until a fix has shipped.
- Include what you found, how to reproduce it, and what an attacker gains.

You'll hear back within 3 working days. We'll agree on a fix and a disclosure date with you, and credit you in the advisory unless you'd rather not be named.

## Scope

- The sync client: [`src/sync/`](src/sync/), including frame sealing and parsing, the vault key and the Keychain, and same-network sync.
- Anything that sends data off this Mac, or lets another Mac or the relay change what's stored here.
- The protocol is specified in ozenhq/sync's [docs/protocol.md](https://github.com/ozenhq/sync/blob/main/docs/protocol.md).

Out of scope: attacks that need the user's unlocked Mac or their vault key already, denial of service by flooding the relay from many hosts, and findings in third-party dependencies with no path to exploit them here (report those upstream).

## Maintainers

Private vulnerability reporting can only be turned on once a repo is public. Turn it on in Settings → Code security → Private vulnerability reporting when this repo is made public.
