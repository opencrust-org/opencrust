---
name: summarize
description: Summarize text, URLs, or files. Produce concise summaries at adjustable length.
triggers:
  - summarize
  - tldr
  - recap
  - summary
dependencies: []
---

# Summarize

When the user asks you to summarize content, follow this process:

## Determine the source

- **URL**: Use the `web_fetch` tool to retrieve the page content.
- **File path**: Use the `file_read` tool to read the file contents.
- **Inline text**: Work directly with the text provided in the message.

## Produce the summary

1. Read the full content before summarizing. Do not summarize incrementally.
2. Identify the key points, arguments, or findings.
3. Preserve the original structure (if the source has sections, reflect them).
4. Default to a short summary (3-5 bullet points). If the user asks for more detail, expand.

## Output format

- **Short** (default): 3-5 bullet points capturing the essential takeaways.
- **Medium**: 1-2 paragraphs with supporting details.
- **Long**: Section-by-section breakdown preserving the document's structure.

If the user says "tldr" or "recap", use the short format. If they say "summarize in detail", use long.

## Rules

- Never fabricate information not present in the source.
- If a URL fails to fetch, tell the user and suggest they paste the content directly.
- For very long content (>10,000 words), summarize in sections rather than as one block.
- Always mention the source at the top of your summary (URL, filename, or "from your message").
