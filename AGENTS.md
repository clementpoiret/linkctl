# AGENTS.md

## Repository rules

- This software isn't released yet: backward compatibility and preservation of legacy code isn't recommended.
- This repository uses [devenv](https://devenv.sh). Use it to manage the development environment and its available
  tools.
- This repository defines the specifications of the `linkctl` tool in a dedicated `specs/` folder; always read the specs
  before making changes.
- Never commit the specs; always keep this folder in the `.gitignore`.
- The specs define a phased implementation plan too; never mention the current phase in the tests, code, or
  documentation.
- If you need the camera to be plugged in, and it is not, ask the user to plug the camera, and pause until it's done.
- Always keep the doc and README.md up-to-date.
- Always describe the jujutsu revision you are working in for your current task (e.g.,
  `jj describe -m "feat: some conventional commit message"`).
