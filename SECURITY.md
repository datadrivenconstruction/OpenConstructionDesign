# Security policy

## Reporting a vulnerability

Please do not report security problems in public issues, discussions or pull requests.

Use GitHub private vulnerability reporting: open the **Security** tab of this repository and
choose **Report a vulnerability**. This creates a private security advisory that only the
maintainers can see.

Include:

- the affected version or commit and your platform;
- steps to reproduce, and a minimal input file if the problem is triggered by a file
  (only files you are allowed to share);
- the impact as you understand it.

Please allow us reasonable time to fix the problem before you disclose it publicly.

You will get an acknowledgement in the advisory thread. We will keep you informed while the
problem is investigated and fixed, and we will credit you in the advisory if you wish.

## Supported versions

Version 0.9.x is the supported line. Fixes are made on `main` and released as new 0.9.x
versions; there are no maintained older release lines.

## Areas of particular interest

- file importers (IFC, `.dwg`, `.dxf`, STEP, glTF, BCF, XLSX) handling untrusted files;
- the bridge host and its process boundary;
- the AI assistant's network egress policy and API key storage;
- the MCP server;
- the script sandbox (JSON scripts and Lua geometry scripts).

## Scope

Reports about third-party dependencies are welcome; we will pass them upstream where needed.
Problems that need local administrator access or a modified build of the program are out of
scope.
