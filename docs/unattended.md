# Agents with nobody at the keyboard

[Español](unattended.es.md) · [Back to the README](../README.md)

CI jobs, servers, containers and cloud agents can use the same memory as the people on the team. Nobody answers prompts there and there may be no home folder, so `omoi` takes everything from environment variables.

## The variables

| variable | what it does |
|---|---|
| `OMOIKANE_HOME` | the whole Omoikane folder: database, models, the meaning model's choice and service, updates. Default `~/.omoikane`. Sync settings, keys and clones, backups and the diary live next to the database, so they go with it. `OMOIKANE_DB` and `OMOIKANE_MODELS` still win for their own part. It must be an absolute path (`~` and `~/…` are expanded); a relative one is an error |
| `OMOIKANE_NONINTERACTIVE=1` | nobody is there: no prompts (whatever needs confirming asks for `--yes`), no model download nobody asked for, no automatic update, no background sync left running after the command (`OMOIKANE_SYNC_AUTO=1` brings it back on a server) |
| `OMOIKANE_SYNC_CODE` | one or more codes from `omoi sync code` (separated by spaces, commas or lines: the general repository and each space's). `omoi sync`, `omoi sync --pull` and `omoi sync join` without a code join them by themselves, sharing the defaults (no preferences) |
| `OMOIKANE_SYNC_URL` | another URL for the code's repository, for instance HTTPS with a CI token (only with a single code). If it changes (a new token), the next `omoi sync` uses the new one |
| `OMOIKANE_AUTHOR` | who signs what this agent saves: the author of each memory and of each verdict (over the one chosen when sync was set up) |
| `OMOIKANE_NO_MODEL=1` | besides the installers, `omoi` itself doesn't download the meaning model when the memory grows |

Without a home folder and without `OMOIKANE_HOME` or `OMOIKANE_DB`, `omoi` stops with an error instead of creating a database in the folder it runs in, which could end up inside the repository being tested.

The code carries the repository's key. `omoi` never writes it out: not on joining, not in errors, and a URL's user and password are hidden too. Keep it in your CI's secrets, and if an agent tries to save one as a memory, it is saved as `[redacted:omoikane-sync-code]`. A space's code only opens that client's repository. One thing to know: `omoi sync code` on a computer that joined with `OMOIKANE_SYNC_URL` hands out that URL, token included, so take codes from a person's computer, not from the CI.

With `OMOIKANE_NONINTERACTIVE=1`, `omoi sync --pull` waits for any other sync, does a full one and exits with an error (code 1) when the repository can't be reached or the key doesn't open it. Without it, `--pull` is the quiet one the hooks launch, which tries again later.

A CI starts from an empty database every run. The folder it checks out is new to it, but the space's repository says that project belongs to the client. On a person's computer that pauses the project until they confirm it (`omoi projects keep`): a client's repository could claim a folder that isn't theirs. In a CI there is nobody to confirm. With `OMOIKANE_NONINTERACTIVE=1` and the space's code in `OMOIKANE_SYNC_CODE`, the variable is the confirmation, and a checkout whose git `origin` is exactly that of one of the space's projects joins the space, brings its memories and uploads what it saves. All four must hold: nobody at the keyboard, that space's repository joined with a code from the variable, a git remote on the project (without one, a folder is only a name) and no memories of it saved on that computer before. If any fails, it is paused as usual and what is saved in it stays on that computer.

Not yet unattended-aware, coming in 1.7.1: the `/api/sync` endpoint of `omoi serve` and the meaning model's service.

## GitHub Actions

A job that joins a client's space, brings its memories and asks them something. Put the code from `omoi sync code --space acme` in the secret `OMOIKANE_SYNC_CODE`, and a deploy key with access to the memory repository in `MEMORY_DEPLOY_KEY`.

```yaml
name: Project memory
on: pull_request

jobs:
  memory:
    runs-on: ubuntu-latest
    env:
      OMOIKANE_NONINTERACTIVE: "1"
      OMOIKANE_SYNC_CODE: ${{ secrets.OMOIKANE_SYNC_CODE }}
      OMOIKANE_AUTHOR: ci:${{ github.repository }}
    steps:
      - uses: actions/checkout@v4

      - name: Install omoi
        run: |
          echo "OMOIKANE_HOME=$RUNNER_TEMP/omoikane" >> "$GITHUB_ENV"
          curl -fsSL https://raw.githubusercontent.com/marcmayol/omoikane/master/install.sh \
            | OMOIKANE_NO_SETUP=1 OMOIKANE_NO_MODEL=1 OMOIKANE_INSTALL_DIR="$RUNNER_TEMP/bin" sh
          echo "$RUNNER_TEMP/bin" >> "$GITHUB_PATH"

      - uses: webfactory/ssh-agent@v0.9.0
        with:
          ssh-private-key: ${{ secrets.MEMORY_DEPLOY_KEY }}

      - name: Join the space and bring its memories
        run: omoi sync

      - name: Ask the memory
        run: |
          omoi search "deploy migrations"
          USER_PROMPT="What should I know before touching the deploy?" omoi recall --format plain
```

Over HTTPS instead of SSH, leave out the deploy key and add `OMOIKANE_SYNC_URL: https://x-access-token:${{ secrets.MEMORY_REPO_TOKEN }}@github.com/acme/memory.git` to `env`. If the job saves memories too (`omoi remember`, or an agent with the MCP), end it with another `omoi sync`: without anybody there nothing syncs in the background. Search works by words without the light model; `omoi model install` adds it (about 150 MB, worth caching `$OMOIKANE_HOME/models`).
