# Security policy

## Supported versions

| Version | Supported |
|---|---|
| 1.0.x | Yes |
| Earlier than 1.0 | No |

## Reporting a vulnerability

Use GitHub private vulnerability reporting: open the repository's **Security** tab, choose **Report a vulnerability**, and describe the problem. Please do not open a public issue or pull request for it.

Include the version, the operating system, the steps to reproduce, and what an attacker gains. Do not include your own data, keys, or a real person's information.

JobPilot AI is one person's personal, non-commercial project. There is no bounty and no response-time guarantee. Reports are read and answered as time allows.

## What is already documented

- The [threat model](docs/security/threat-model.md) lists the controls and the residual risks that are known and accepted, such as the word-list stops in Claude Code apply mode.
- The [1.0 security review](docs/security/review-1.0.md) lists the findings and what became of each.

A report about a documented, accepted residual is still welcome, but it is not a new finding unless it shows the control is weaker than the threat model says.
