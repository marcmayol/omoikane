# Recall and search

[Español](recall.es.md) · [Back to the README](../README.md)

How Omoikane hands the agent what it needs without being asked, how it keeps quiet the rest of the time, and how search works.

## It recalls by itself

When a session starts, the agent gets your preferences and pinned memories, where each project was left, what was worked on last and anything worth reviewing. On each prompt it only gets something that really matters: a memory has to cover enough of the prompt's rarer words, and gotchas also come up when you name their files. Each memory arrives once per session. Over 300 real prompts it injected something into 7 % of them. If 15 minutes and 10 prompts go by with nothing saved, it reminds the agent to save.

It picks up where you left off: each project's latest `status` memory arrives at session start, so "let's carry on with X" needs no searching.

## Noise, stale memories and mistakes

A recall that injects too much is worse than none: it fills the context with noise and carries what no longer holds from one session to the next. That is why it is the most heavily calibrated part of Omoikane.

- **Mostly silent.** A prompt only gets something if it has at least 3 useful words and a memory shares 2 words that are rare in your base, adding up to at least half of the prompt's weight. The meaning channel, for paraphrases, also needs the words to cover 25 % and is skipped on prompts longer than 500 characters. Over 300 real prompts it injected something into 7 %; the rest got nothing. "How did it all go?" brings nothing.
- **Little, and once.** At most 2 memories per prompt, and only those close to the best one: if one stands out, it goes alone. Each arrives as 200 characters; if the agent wants the rest, it calls `get`. A linked gotcha or preference may join them, and a procedure only when the prompt says one of its `when:` words. Each memory arrives once per session.
- **Stale memories arrive flagged.** If a newer memory on the same subject contradicts it (another version of the same thing, or done what was pending), the old one arrives with `⚠ may be outdated: newer <id> …` in front, and the agent checks the new one before trusting it. It also flags project statuses left behind, paths that no longer exist and memories nobody uses. The Review tab puts both memories side by side to forget the old one or keep it. No model involved: words, dates and versions.
- **Mistakes get fixed, not carried.** Only what the agent saves goes into memory. What Omoikane sets aside when a session closes are proposals, and they expire after 7 days if nobody saves them. Fixing one is `update` with a reason, and the previous version stays in the history. `forget` deletes it, and it doesn't come back from your other machines. What a teammate saved arrives marked `[by Ana]`.
- **Measured.** Every threshold comes from questions written blind by another agent, 20 of them with no answer in the notes: on those it injected something twice. The numbers are in [BENCHMARK.md](../BENCHMARK.md).

At session start more does arrive: your preferences, what you pinned, where each project stands and what was touched last. Preferences and pins are what you want the agent to always know.

engram, the alternative Omoikane is compared with, doesn't inject memories on each prompt. With stale ones it does something else: every save proposes similar memories and asks the agent to judge how they relate (`mem_judge`). Omoikane asks for nothing. It detects the contradiction itself, flags it in the recall with the reason, and lets the agent judge only when it knows something Omoikane can't see (`supersedes`, `contradicts`, `compatible`). How those verdicts work is in [memory-care.md](memory-care.md#verdicts).

## It understands what you ask with other words

Words find a memory when you ask with the words it uses. When you ask the same thing differently ("how do I bring back a release I pulled?" for a memory about `cargo yank --undo`), words don't find it, and the light model rarely does. From 1.2 a second model, [EmbeddingGemma 300M](https://huggingface.co/google/embeddinggemma-300m), compares meaning:

- **In search**, each candidate scores its similarity in meaning plus a share of its word score; with five words or fewer (keywords) words weigh more. On the public benchmark, with 3,000 memories, the right memory comes first for 55 % of paraphrases (9 % without it, 6 % for engram), 89 % of questions and 96 % of keyword searches.
- **In recall it mostly keeps quiet**: what the words bring stays out if it isn't among the three memories closest in meaning, and the closest one comes in when it clearly stands out. On questions with no answer, recall injects something 8 % of the time with 3,000 memories and 15 % with 10,000 (40 % and 70 % without it). Both rules are relative, a position and a lead, because similarity depends on the language and on how people write.
- **What it costs**: about 60 ms more per search and per prompt (never more than 250 ms in recall: past that it goes on without it), about 450 MB on disk, and computing each memory's vector once, about a quarter of a second each, in the background.
- **How it runs**: in pure Rust inside `omoi`, on the CPU. Loading it takes 1.4 s, so a service of its own keeps it loaded (one per user, `127.0.0.1` only, with a token) and closes after 30 minutes without use. Nobody waits for it to load: until it's there, and until most memories have their vector, everything works by words as before. `omoi doctor` shows how many have one.
- **Installing and turning it off**: `omoi model install --semantic` downloads it and checks every file's SHA-256. `omoi model semantic --off` goes back to the light model only (faster, less memory) and closes the service; `--on` turns it back on.
- **RAM while indexing**: the service computes the vectors of new memories in batches; `omoi model semantic --ram <MB>` caps the RAM those batches use (more is a bit faster; `0` goes back to the default). `omoi model semantic` and `omoi doctor` show the cap in use.

## It searches in your language

Search starts with words: accents don't matter, file paths count too, and rare words weigh more. Then a local embedding model, which opens in 2.7 ms, finds what is very close in meaning even with no word in common. Where it searches depends on where you are: from a project, that project and the global memories; from your home folder, everything.

It works in 101 languages, accents or not. On questions written blind it finds the right memory first 90 % of the time and within the top three 97 %, with a median of 43 ms, and stays as accurate at 3,183 memories (50 ms per search). Four processes share the model at 13 MB of memory each. The model is [potion-multilingual-128M](https://huggingface.co/minishlab/potion-multilingual-128M) (int8), and it opens in 2.7 ms instead of 1.78 s. Until it has downloaded (~150 MB, once), search works by words.

## What about CLAUDE.md?

For a small project with twenty things to remember, a good CLAUDE.md is enough. Omoikane pays off where CLAUDE.md stops reaching:

- CLAUDE.md is loaded whole every session, relevant or not, and the more it knows the more context it spends. Omoikane brings what fits the prompt.
- It's one file per project. What you learn in one doesn't reach another unless you copy it; in Omoikane a memory can apply everywhere.
- It doesn't know when something has gone stale, nor keep why it changed.
- You write it. Omoikane gets filled by the agent as it works.
- One agent reads it. Omoikane is shared by 15, and by all your machines.

They can live together: CLAUDE.md for the repository's fixed rules, Omoikane for what gets learned along the way.
