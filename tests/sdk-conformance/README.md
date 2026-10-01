# SDK conformance suite (the review's §7)

13 category dirs, one per §7. The language-neutral vectors live in
`protocol/test-vectors/` (frozen, gate-checked); every SDK executes the
same vectors through `sdk-conformance --language {rust,python,go,typescript,java}`
— the runner lands with D-11.
