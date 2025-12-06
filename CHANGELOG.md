# Changelog

## v0.0.2
- Add macOS single-call cloning strategy plus benchmarks and strategy selection wiring.
- Preserve symbolic links during cloning (recreated with original targets).
- Preserve empty directories during cloning.
- Remove `Options::overwrite()` — destination must not exist (error if it does).
- Add `Error::Walk` variant for better walk error handling.
- Update dependencies, tighten lints/docs, add AGENTS guidance, and refresh README.
- Document errors/options, simplify benchmark entrypoint, and resolve clippy warnings.

## v0.0.1
- Initial release.
