@echo off
rem Stands in for the real `claude` in tests. Each test's wrapper script sets MOCK_CLAUDE_*.
rem Differences from the sh mock: no pid (cmd has no $$), and %* lands on one line of the args file.
setlocal
if "%~1"=="--version" goto :version
if defined MOCK_CLAUDE_CWD cd >"%MOCK_CLAUDE_CWD%"
if not defined MOCK_CLAUDE_SLEEP goto :record
set /a PINGS=%MOCK_CLAUDE_SLEEP%+1
ping -n %PINGS% 127.0.0.1 >nul
if defined MOCK_CLAUDE_DONE type nul >"%MOCK_CLAUDE_DONE%"
:record
if defined MOCK_CLAUDE_ARGS >"%MOCK_CLAUDE_ARGS%" echo(%*
if defined MOCK_CLAUDE_STDIN findstr "^" >"%MOCK_CLAUDE_STDIN%"
if defined MOCK_CLAUDE_ENV set >"%MOCK_CLAUDE_ENV%"
if defined MOCK_CLAUDE_REPLY goto :reply
echo {"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"ok":true},"permission_denials":[]}
goto :done
:reply
type "%MOCK_CLAUDE_REPLY%"
:done
if not defined MOCK_CLAUDE_EXIT set MOCK_CLAUDE_EXIT=0
exit /b %MOCK_CLAUDE_EXIT%
:version
if defined MOCK_CLAUDE_VERSION_SLEEP ping -n %MOCK_CLAUDE_VERSION_SLEEP% 127.0.0.1 >nul
echo 2.1.316 (mock)
if not defined MOCK_CLAUDE_VERSION_EXIT set MOCK_CLAUDE_VERSION_EXIT=0
exit /b %MOCK_CLAUDE_VERSION_EXIT%
