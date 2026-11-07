# Recall and search

[Español](recall.es.md) · [Back to the README](../README.md)

How Omoikane hands the agent what it needs without being asked, how it keeps quiet the rest of the time, and how search works.

## It recalls by itself

When a session starts, the agent gets your preferences and pinned memories, where each project was left, what was worked on last and anything worth reviewing. On each prompt it only gets something that really matters: a memory has to cover enough of the prompt's rarer words, and gotchas also come up when you name their files. Each memory arrives once per session. Over 300 real prompts it injected something into 7 % of them. If 15 minutes and 10 prompts go by with nothing saved, it reminds the agent to save.

It picks up where you left off: each project's latest `status` memory arrives at session start, so "let's carry on with X" needs no searching.

Project names are understood even when misspelled. Wherever you write one (`project`, `--project`, `omoi projects`), case, accents, hyphens, underscores, spaces and dots don't matter (`cronicas` finds `Crónicas`), and if nothing else fits, one letter off is enough (`Omoikae`). If it fits several, it asks which. In a prompt, a misheard name ("tokimicho" for Tokimichi, "onmoikane" for Omoikane) is searched as the right name, as long as the word is in no memory, both have at least 6 letters, the word isn't the name with an ordinary ending ("faltas" is not Faltis) and the project is one you can see from where the session is (from a client's project, only that client's projects; never another client's). With 5 letters, ordinary words became projects ("tropa" Troba, "rodeo" Rodet, "balsa" Balùa), so a 5-letter name ("trova" for Troba) is no longer guessed.

## Noise, stale memories and mistakes

A recall that injects too much is worse than none: it fills the context with noise and carries what no longer holds from one session to the next. That is why it is the most heavily calibrated part of Omoikane.

- **Mostly silent.** A prompt only gets something if it has at least 3 useful words and a memory shares 2 words that are rare in your base, adding up to at least half of the prompt's weight. The meaning channel, for paraphrases, also needs the words to cover 25 % and is skipped on prompts longer than 500 characters. Over 300 real prompts it injected something into 7 %; the rest got nothing. "How did it all go?" brings nothing.
- **Little, and once.** At most 2 memories per prompt, and only those close to the best one: if one stands out, it goes alone. Each arrives as 200 characters; if the agent wants the rest, it calls `get`. A linked gotcha or preference may join them, and a procedure only when the prompt asks for it: a phrase from its `when:` line, or one of its words plus something that says it is that procedure (a word of its title, its project, or being in its project), so "publish a post" doesn't bring "Publish a release". Each memory arrives once per session.
- **Stale memories arrive flagged.** If a newer memory on the same subject contradicts it (another version of the same thing, or done what was pending), the old one arrives with `⚠ may be outdated: newer <id> …` in front, and the agent checks the new one before trusting it. It also flags project statuses left behind, paths that no longer exist and memories nobody uses. The Review tab puts both memories side by side to forget the old one or keep it. No model involved: words, dates and versions.
- **Mistakes get fixed, not carried.** Only what the agent saves goes into memory. What Omoikane sets aside when a session closes are proposals, and they expire after 7 days if nobody saves them. Fixing one is `update` with a reason, and the previous version stays in the history. `forget` deletes it, and it doesn't come back from your other machines. What a teammate saved arrives marked `[by Ana]`.
- **Measured.** Every threshold comes from questions written blind by another agent, 20 of them with no answer in the notes: on those it injected something twice. The numbers are in [BENCHMARK.md](../BENCHMARK.md).

At session start more does arrive: your preferences, what you pinned, where each project stands and what was touched last. Preferences and pins are what you want the agent to always know. The first session in a client's project also tells it what the rest of that client's team has ([Someone new joins a client](sync-and-spaces.md#someone-new-joins-a-client)).

engram, the alternative Omoikane is compared with, doesn't inject memories on each prompt. With stale ones it does something else: every save proposes similar memories and asks the agent to judge how they relate (`mem_judge`). Omoikane asks for nothing. It detects the contradiction itself, flags it in the recall with the reason, and lets the agent judge only when it knows something Omoikane can't see (`supersedes`, `contradicts`, `compatible`). How those verdicts work is in [memory-care.md](memory-care.md#verdicts).

## It understands what you ask with other words

Words find a memory when you ask with the words it uses. When you ask the same thing differently ("how do I bring back a release I pulled?" for a memory about `cargo yank --undo`), words don't find it, and the light model rarely does. From 1.2 a second model, [EmbeddingGemma 300M](https://huggingface.co/google/embeddinggemma-300m), compares meaning. It only comes in when the memory needs it:

- **Light by default.** With a small memory, words alone are as good: below 1,000 memories the benchmark sees little difference in recall. So below **1,000 memories** Omoikane works as 1.1 did, about 35 ms per prompt, without downloading the model or keeping a service running. Once the memory passes 1,000, Omoikane downloads it by itself in the background (about 450 MB), indexes the memories and says so once at the start of a session. Every memory in the database counts, not just the current project's: the model, its service and its vectors belong to the whole database, so the decision is the same from every folder.
- **Your choice wins over the size.** `omoi model semantic --on` uses it even with a small memory, `--off` never (and closes its service), `--auto` lets the size decide again. `omoi model semantic` and `omoi doctor` say whether it is in use and why.
- **In search**, always (when it is in use): each candidate scores its similarity in meaning plus a share of its word score; with five words or fewer (keywords) words weigh more. On the public benchmark, with 3,000 memories, the right memory comes first for 55 % of paraphrases (9 % without it, 6 % for engram), 89 % of questions and 96 % of keyword searches. What only meaning brings, with no word in common, needs a similarity of 0.35: a search can come back empty instead of showing something unrelated.
- **In recall, only to keep quiet**: it is asked only when the words already brought something, on a prompt of more than five words and at most 300 characters, and what the words bring stays out if it isn't among the three memories closest in meaning. Preferences are never taken out: a preference applies without resembling the prompt. On questions with no answer, recall injects something 7.5 % of the time with 3,000 memories (40 % without it). It is asked on few prompts (60 of 1,161 of the author's prompts), so recall stays close to 35 ms.
- **When it doesn't answer in time** (250 ms in recall, for example with the CPU busy), recall goes on with words only, as in 1.1. It isn't silent: `omoi doctor` shows how many times recall asked it and how many got no answer, and `OMOIKANE_TRAZA=1` writes every step of a recall to stderr.
- **What it costs**, measured on the public benchmark: search goes from ~32 to ~64 ms; recall from ~27-31 ms to ~32 ms with 3,000 memories and ~54 ms with 10,000 (most prompts don't ask it). About 450 MB on disk, and computing each memory's vector once in the background (450 s for 3,000 short notes on 8 CPU threads; long memories take longer).
- **How it runs**: in pure Rust inside `omoi`, on the CPU, with its own engine (f16 weights read straight from disk, AVX2; CPUs without AVX2 use candle). It loads in about 26 ms and uses about 13 MB of its own RAM, because the weights stay in the file. A service of its own keeps it loaded (one per user, `127.0.0.1` only, with a token) and closes after 30 minutes without use. Nobody waits for it to load: until it's there, and until most memories have their vector, everything works by words as before. `omoi model install --semantic` downloads it right away and checks every file's SHA-256.
- **RAM while indexing**: the service computes the vectors of new memories in batches; `omoi model semantic --ram <MB>` caps the RAM those batches use (more is a bit faster; `0` goes back to the default). `omoi model semantic` and `omoi doctor` show the cap in use.

## It searches in your language

Search starts with words: accents don't matter, file paths count too, and rare words weigh more. Then a local embedding model, which opens in 2.7 ms, finds what is very close in meaning even with no word in common. Where it searches depends on where you are: from a project, that project and the global memories; from your home folder, everything.

It works in 101 languages, accents or not. On questions written blind it finds the right memory first 90 % of the time and within the top three 97 %, with a median of 43 ms, and stays as accurate at 3,183 memories (50 ms per search). Four processes share the model at 13 MB of memory each. The model is [potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M) (int8), and it opens in 2.7 ms instead of 1.78 s. Until it has downloaded (~150 MB, once), search works by words.

## It reads your docs too

If the project already keeps its documentation in Markdown (a `docs/` folder, an LLM wiki, an Obsidian vault), tell Omoikane where it is:

```sh
omoi docs add docs                               # indexes it now
omoi docs list
omoi docs test "how do I roll back a deploy?"    # what recall would bring for that
omoi docs remove docs
```

From then on, next to the memories, recall can bring the section of that documentation that fits the prompt: one at most, about 700 characters with its file and heading, each once per session. If the agent needs more, it opens the file. The rule is the one memories follow: the section has to share two words that are rare in the documentation and cover 60 % of the prompt's weight (a little more than a memory, which needs half, because a section is longer), so a prompt that isn't about the documentation gets nothing from it.

Nothing is copied into memory and the files are never written. Omoikane keeps an index of them, split by headings (`#`, `##`, `###`), of regular `.md` and `.markdown` files only (symbolic links aren't followed, and files over 2 MB are left out), and brings it up to date when a session starts, reading only the files whose date or size changed. That runs on the side, so the session doesn't wait for it, even after switching to a branch where hundreds of files changed; until it finishes, recall uses the index as it was, and no other refresh of the project starts. A drive root or your home folder can't be added: point to the documentation folder itself. The folders belong to the project you run `omoi docs add` from, and both they and the index stay on this computer: they aren't synced or exported. A project without docs pays nothing for it.

## What about CLAUDE.md?

For a small project with twenty things to remember, a good CLAUDE.md is enough. Omoikane pays off where CLAUDE.md stops reaching:

- CLAUDE.md is loaded whole every session, relevant or not, and the more it knows the more context it spends. Omoikane brings what fits the prompt.
- It's one file per project. What you learn in one doesn't reach another unless you copy it; in Omoikane a memory can apply everywhere.
- It doesn't know when something has gone stale, nor keep why it changed.
- You write it. Omoikane gets filled by the agent as it works.
- One agent reads it. Omoikane is shared by 15, and by all your machines.

They can live together: CLAUDE.md for the repository's fixed rules, Omoikane for what gets learned along the way.
