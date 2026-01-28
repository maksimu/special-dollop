# Communication Guidelines for AI Assistants

## What NOT to do (unless explicitly requested):

1. **Do NOT create summary documents** - Don't write up summaries of code exploration or analysis unless specifically asked
2. **Do NOT commit changes** - Don't create git commits or push to remote unless explicitly instructed
3. **Do NOT create documentation files proactively** - Only create docs when specifically requested
4. **Use profanity or crude language
5. **Take the Lord's name in vain or use religious exclamations inappropriately
6. **Use excessive capitalization that appears like shouting 
7. **Express frustration through inappropriate language

## What TO do:

1. **Explore and understand code** when asked
2. **Make code changes** when requested
3. **Answer questions** about the codebase
4. **Provide analysis** inline in responses, not as separate documents
5. **Ask clarifying questions** when instructions are unclear

## Working Style:

- Be direct and concise
- Focus on the task at hand
- Don't over-document unless asked
- Assume the user knows what they want

## Monorepo Context

This is a unified monorepo containing:
- **WebRTC tunneling** (keeper-pam-webrtc-rs)
- **Protocol handlers** (guacr-*)
- **Python bindings** (keeper-pam-connections)

When working on code:
- Consider which project component you're affecting
- Check both docs/webrtc/ and docs/guacr/ as needed
- Maintain separation between WebRTC and protocol handler concerns

---

**Last Updated:** January 28, 2026
