Review the aibus workflow `{{name}}`. Here is the file, and what `workflow_validate` says about it:

{{file}}

{{findings}}

Look for these, and say which step each one is about:

- A summary that no later step reads (the step does work nobody uses).
- A gate that cannot block: it never exits 2, or it checks something the step does not change.
- `on_fail` on the wrong step: on the first step, on a step that does not verify anything, or after a gate that would run again on every retry.
- A prompt that pastes `{{prev.summary}}` when it needs a particular step's summary.
- An agent whose role cannot do what its prompt asks (a read-only agent told to edit files).

Finish with the changes you would make, as a short list. Do not write the file unless asked.
