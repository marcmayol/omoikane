# Security

Omoikane holds what people and teams learned while they worked, and syncs it through git repositories encrypted with their own keys. A flaw that leaks a memory, lets one client's data reach another, weakens the encryption, or lets an update run a binary that wasn't signed is treated as the most urgent kind of bug.

## Reporting a problem

Please **don't open a public issue**. Report it privately from the repository's **Security** tab → **Report a vulnerability** (GitHub's private reporting). Include what you did, what you expected and what happened, and the version (`omoi --version`) and system.

You'll get an answer within a week. Once it's fixed, the release notes say what was fixed and credit you, unless you'd rather not be named.

## Supported versions

| Version | Security fixes |
|---|---|
| Latest 1.x | Yes |
| Older 1.x | Update to the latest 1.x: it syncs with them and opens their database |
| 0.x | No |

## What's in scope

- Memories, verdicts or projects of one space reaching another space's repository, search or recall.
- Anything readable in a sync repository or an encrypted export without the key.
- Secrets that the redaction should have caught reaching the database or a sync repository.
- The automatic update accepting a binary without a valid Ed25519 signature, or the installers and the plugin accepting one with a wrong SHA-256.
- `omoi serve` (it only listens on 127.0.0.1) reachable from outside the machine, or from a web page through DNS rebinding.

Out of scope: someone who already has your user account and your keys, and memories the user saved on purpose.
