## Git Commit Style Preferences

**NEVER commit unless explicitly asked by the user.**

When committing: review `git diff`

- Use conventional commit format: `type: subject line`
- Keep subject line concise and descriptive
- **NEVER include marketing language, promotional text, or AI attribution**
- **NEVER add "Generated with Claude Code", "Co-Authored-By: Claude", or similar spam**
- Follow existing project patterns from git log
- Prefer just a subject and no body, unless the change is particularly complex

## Tone and Communication

- ASCII only. No em dashes, smart quotes, or other unicode punctuation. Use "--"
  only in code contexts, not as prose punctuation.
- No wasted words. No fluff. Each word should add value to the reader.
- Human readable and clear. Prefer short sentences, one idea each. Break a
  clause-stacked sentence into two or three. After drafting, re-read and cut every
  word that does not add information.
- Calm, matter-of-fact technical tone.
- Avoid the verbless appositive cadence: a fragment that drops the verb and stacks
  noun phrases for rhythm. Examples to NOT write: "Two streams, both
  newline-delimited JSON.", "one answer, two surfaces, both substantial.", "No
  TUI, no daemon.". It performs crispness instead of stating the fact. The tell is
  a comma or semicolon sitting where the verb belongs, often opening or closing a
  paragraph. Restore the verb: "Both streams are newline-delimited JSON."

## Nushell services

The pty is wrapped as a cross.stream duplex service in `serve.nu`. The external
command must be the HEAD of the closure pipeline:

```nushell
run: {|| ^ptyZZZ run -- nu | lines | each { ... } }
```

Do NOT write `$in | ^ptyZZZ run -- nu`. `$in` collects its input before passing
it on, and a duplex service's input stream never ends, so `$in` blocks forever and
the external command never spawns. A duplex service feeds its `<name>.send` frames
straight to the first command's stdin, so the external belongs at the head with no
`$in`.
