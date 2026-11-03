@echo off
rem Stands in for an interactive `claude` apply session in tests: the node script beside this
rem file does the work. Each test's wrapper sets JOBPILOT_FAKE_* and JOBPILOT_NODE_BIN.
setlocal
if defined JOBPILOT_NODE_BIN (
  "%JOBPILOT_NODE_BIN%" "%~dp0claude" %*
) else (
  node "%~dp0claude" %*
)
exit /b %ERRORLEVEL%
