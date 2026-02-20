---
name: web-lookup
description: Fetch and distill information from URLs. Summarize web pages, extract key facts, answer questions from web content.
triggers:
  - look up
  - search for
  - what is
  - check this link
  - fetch
dependencies: []
---

# Web Lookup

When the user asks you to look something up or provides a URL, fetch and distill the information.

## How to look up

1. **URL provided**: Use `web_fetch` to retrieve the page content.
2. **Topic/question provided**: If the user asks "what is X" or "look up Y", explain that you can fetch a specific URL if they provide one, or answer from your knowledge.

## Processing the content

After fetching a URL:

1. **Extract the relevant information** - don't dump the entire page. Focus on what the user asked about.
2. **Structure the response**:
   - Key facts or findings (bullet points)
   - Relevant quotes or data points
   - Source attribution (the URL)
3. **Answer the user's question** if they asked one specific thing.

## Output format

For general lookups:
> **[Page title or topic]** - [source URL]
>
> [Key findings in 3-5 bullet points]

For specific questions:
> [Direct answer to the question]
>
> Source: [URL]

## Handling multiple URLs

If the user provides multiple URLs or asks you to compare sources:
1. Fetch each URL separately.
2. Present findings from each source.
3. Note agreements and contradictions between sources.

## Rules

- Always cite the source URL.
- If `web_fetch` fails (timeout, 404, paywall), tell the user and suggest alternatives.
- Don't fabricate information. If the page doesn't contain what the user is looking for, say so.
- For very long pages, focus on the most relevant sections rather than summarizing everything.
- If the content is behind a login or paywall, let the user know you can't access it.
