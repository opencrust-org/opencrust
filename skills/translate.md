---
name: translate
description: Translate text between languages. Auto-detects source language.
triggers:
  - translate
  - in spanish
  - in arabic
  - in french
  - to english
  - translation
dependencies: []
---

# Translate

Translate text between languages when the user asks.

## How to translate

1. **Detect the source language** from the text (or the user will specify it).
2. **Determine the target language** from the user's request ("translate to Spanish", "in Arabic", etc.).
3. **Translate** preserving meaning, tone, and context.

## Output format

> **[Source language] → [Target language]**
>
> [Translated text]

If the text is long, preserve paragraph structure. If it contains technical terms, keep them and add a note if the translation is ambiguous.

## Rules

- Preserve the original meaning. Don't paraphrase unnecessarily.
- Keep formatting (bullet points, headers, code blocks) intact.
- For ambiguous words, pick the contextually appropriate translation and note the alternative if it matters.
- If the user provides a file path, use `file_read` to get the content, translate it, and present the result.
- If the user provides a URL, use `web_fetch` to get the content first.
- For code comments, translate the comments but leave the code untouched.
- If you're not confident in a translation (rare language, domain-specific jargon), say so.

## Common requests

- "Translate this to [language]" — straightforward translation
- "What does this say?" — detect language, translate to English (or the user's primary language)
- "How do you say [phrase] in [language]?" — provide the translation with pronunciation guidance if helpful
- "Translate this file" — read the file, translate the content, present the result
