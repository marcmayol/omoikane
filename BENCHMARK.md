# Omoikane vs engram

[Español](BENCHMARK.es.md)

A head-to-head with [engram](https://github.com/Gentleman-Programming/engram) (v3.6.0), the Go + SQLite memory for coding agents that Omoikane was compared against from the start. Measured on 28 October 2026 on one Windows 11 PC. **I wrote Omoikane, so read this with that in mind**; the method below is meant to keep that from tilting the numbers, and every result where engram does better is reported.

## Method

- **Data.** 127 real memory notes from one developer's daily work (Android apps, 3D printing, servers, writing), mostly in Spanish. They are private and not published. The same notes go into both systems, content cut to 6,000 characters for both (engram's CLI only takes the content as an argument, and Windows caps a command line at 32 K).
- **Questions.** 126 one-line summaries of the notes (the developer's own index), plus **80 questions written blind** by a separate agent that only saw the notes, never either system: 20 short keyword-style, 25 natural, 15 paraphrases that avoid the note's own words, and 20 with no answer in the notes.
- **Fair use of engram.** Its update check (one GitHub call per command) was turned off with its own `ENGRAM_NO_UPDATE_CHECK=1`. Search was run three ways: its default (all words must match), `--match any`, and `--match any` with the query cleaned to keywords (no stopwords or words under 4 letters), which is how a careful agent would query it.
- **Timing.** A new process per call for both (that is how agents and hooks call them), interleaved A/B so neither always goes first. Medians.
- **End to end.** 20 of the blind questions answered by Claude with only one memory server available (each system's own MCP server, same instruction, no other tools, plugins and auto-memory off). A second Claude graded every answer against the note **without knowing which system produced it**, in shuffled order.

## Results

### Search (CLI, top 5)

| Questions | Omoikane | engram default | engram `--match any` | engram any, clean keywords |
|---|---|---|---|---|
| 126 summaries: top-1 / top-3 | **98.4 % / 100 %** | 4.0 % / 4.8 % (95 % no result) | 3.2 % / 6.3 % | **99.2 % / 100 %** |
| 20 short: top-1 / top-3 | **90 % / 95 %** | 55 % / 65 % | 30 % / 35 % | 80 % / 85 % |
| 25 natural: top-1 / top-3 | **88 % / 96 %** | 0 % (no result) | 0 % / 4 % | **88 %** / 92 % |
| 15 paraphrases: top-1 / top-3 | 80 % / 93 % | 0 % (no result) | 0 % / 7 % | **87 % / 93 %** |
| 20 with no answer: returns something | 100 % | 0 % | 100 % | 100 % |

engram ranks well once the query is a list of keywords; with the question as a person types it, its default finds nothing and `--match any` falls back (per its docs, short terms switch it to a newest-first LIKE search). Omoikane takes the question as typed. Neither search refuses to answer: both return their closest matches when nothing fits (engram's default returns nothing for most queries, answerable or not).

### Speed (new process per call)

| | Omoikane | engram |
|---|---|---|
| Search, median | **34 ms** | 120 ms (default) · 155 ms (`any`) |
| Save, median | **39 ms** | 135 ms |

### Duplicates: 126 notes saved again as an agent would (another title, the start of the text)

| | Omoikane | engram CLI | engram MCP (`mem_save`) |
|---|---|---|---|
| Flags the original | 90 % (114/126) | 0 % | **100 %** among its candidates (first in 97 %) |
| Flags something for 90 sentences that are not duplicates | **1 %** (1/90) | 0 % | 49 % (44/90) |
| What happens | Doesn't save; the agent updates or forces | Saves | Saves, then asks the agent to judge each candidate (`mem_judge`) |

Two designs: engram always saves and hands the agent BM25 candidates to judge, catching every duplicate at the cost of asking about half of the time when there is none; Omoikane decides itself (vector plus word coverage) and rarely asks in vain, missing 1 in 10.

### Automatic recall (memories injected on each prompt)

Omoikane only: engram's hooks remind the agent to save and load recent context, but don't inject the memories related to each prompt. With the 80 blind questions: the right note for 15/20 short, 18/25 natural and 3/15 paraphrases; something injected for 2 of the 20 questions with no answer. On 300 real prompts from the same developer, 11 % got an injection.

### End to end: Claude answering with each system (20 questions, blind grading)

| | Omoikane | engram |
|---|---|---|
| Correct | 20/20 | 19/20 |
| Median time per answer | 11.7 s | 12.6 s |
| Median turns | 4 | 4.5 |
| Total cost (same model) | **$1.40** | $1.94 |

With an agent in the loop both are good: the agent rewrites questions into keywords for engram. One question apart out of 20 is not a significant difference. The cost gap (+39 % for engram) is consistent with its larger tool surface (27 tools by default) and longer tool results.

## 0.4.0: a second round, on questions never used for tuning

The 80 blind questions above were then used to tune 0.4.0. To check it, another agent wrote **130 new questions blind** (25 short, 35 natural, 25 paraphrases, 15 in English over the Spanish notes, 30 with no answer, asking about details from the middle and end of the notes), which were only run at the end. Another AI reviewed the code; it found that the new hooks for Codex and Gemini wouldn't have run on Windows, which was fixed before release.

### Search on the new questions (127 notes, top 5)

| Questions | Omoikane | engram any, clean keywords |
|---|---|---|
| 25 short: top-1 | 96 % | **100 %** |
| 35 natural: top-1 / top-3 | **86 % / 94 %** | 83 % / 89 % |
| 25 paraphrases: top-1 / top-3 | **92 % / 96 %** | 84 % / 88 % |
| 15 English over Spanish notes: top-1 / top-3 | **87 % / 100 %** | 67 % / 80 % |
| Median time | **34 ms** | 158 ms |

### Automatic recall on the new questions (Omoikane 0.3.1 vs 0.4.0)

| | 0.3.1 | 0.4.0 |
|---|---|---|
| Right note injected, of 100 | 51 | **64** |
| ...of them paraphrases, of 25 | 5 | **14** |
| Something injected, of 30 with no answer | **5** | 7 |

0.4.0 adds a meaning channel to the recall (static embeddings plus a word-coverage floor, short prompts only) and counts only the first 6,000 characters of a memory. A 3,000-character cap looked better on the tuning questions but lost 7 of 35 natural questions here, so it was dropped as overfitting.

### Duplicates: engram's method, without its noise

When Omoikane's own check isn't sure, 0.4.0 saves the memory and returns up to 3 related ones (engram's method, ported: BM25 on the title's words and at least two title words in common).

| | Omoikane 0.4.0 | engram MCP |
|---|---|---|
| Notes saved again that get flagged or listed | **100 %** | **100 %** |
| Related listed for 90 sentences that are not duplicates | **14 %** | 49 % |
| Related listed for 127 notes that are not duplicates | **52 %** | 95 % |

### Scale and English: 3,183 memories

The 127 notes plus 3,055 paragraphs from technical READMEs in English, loaded into both with their own importers; 50 English questions written blind about those paragraphs.

| | Omoikane | engram any, clean keywords |
|---|---|---|
| 80 tuning questions (Spanish): top-1 short / natural / paraphrase | **90 / 88 / 80 %** | 85 / 80 / 80 % |
| 50 English questions: top-1 short / natural / paraphrase | 87 / **95** / 47 % | 87 / 85 / 47 % |
| Search, median | **50 ms** | 203 ms |
| Save, median | **38 ms** | 149 ms |
| Recall per prompt, median | 46 ms | n/a |

Both are weak on English paraphrases (47 % top-1); that is the limit of keyword search and of a static embedding model.

### End to end on the new questions (30, blind grading)

| | Omoikane | engram |
|---|---|---|
| Correct | 30/30 | 29/30 (one "don't know" that added unrelated client details) |
| Median time | 12.4 s | 12.0 s |
| Total cost | **$2.16** | $3.03 |

## 0.5.0: a third, untouched set

For 0.5.0 the recall injects at most 2 memories, only those within 85 % of the best one. A **third set of 100 questions written blind** (30 short, 30 paraphrases, 40 with no answer, a quarter of them reusing the notes' vocabulary) checked it, with another AI judging, blind to the version, whether each injected memory was useful context.

| Recall | 0.3.1 | 0.4.0 | 0.5.0 |
|---|---|---|---|
| Right note injected, of 60 | 32 | **37** | 36 |
| Useful memories injected | 38 | **46** | 44 |
| Noise injected | 23 | 28 | **21** |
| Precision | 62 % | 62 % | **68 %** |

| Search on the third set | Omoikane | engram any, clean keywords |
|---|---|---|
| 30 short: top-1 | 100 % | 100 % |
| 30 paraphrases: top-1 / top-3 / top-5 | **80 %** / 83 % / 90 % | 60 % / **93 % / 97 %** |
| Median time | **35 ms** | 158 ms |

On paraphrases Omoikane puts the right note first more often, but when it misses, engram more often has it in the top 3-5.

## Caveats

- One developer's notes, mostly Spanish, 127 of them. Other corpora (English, thousands of memories, code-heavy) may differ.
- 20 end-to-end questions is a small sample; the search and duplicate numbers use more.
- engram does much more than this measures: TUI, HTTP API, sync and cloud, integrations for 14 agents, conflict review. This compares memory quality and speed, not features.
- Omoikane's thresholds were recalibrated with these same notes the same day: duplicates with the notes, and the recall threshold with the blind questions (checked against the 300 real prompts too), so those two Omoikane rows are optimistic. Search, which drives the search and end-to-end tables, didn't change.
