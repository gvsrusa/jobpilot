@echo off
rem Stands in for the real `codex` in tests. Never reaches a network. Each test's wrapper script
rem sets MOCK_CODEX_*, because the adapter clears the child's environment.
rem Differences from the sh mock: %* lands on one line of the args file.
setlocal
if "%~1"=="--version" goto :version
if defined MOCK_CODEX_CWD cd >"%MOCK_CODEX_CWD%"
if defined MOCK_CODEX_CWD dir /a /b >>"%MOCK_CODEX_CWD%" 2>nul
if defined MOCK_CODEX_ARGS >"%MOCK_CODEX_ARGS%" echo(%*
if defined MOCK_CODEX_STDIN findstr "^" >"%MOCK_CODEX_STDIN%"
if defined MOCK_CODEX_ENV set >"%MOCK_CODEX_ENV%"
set "LAST="
:scan
if "%~1"=="" goto :scanned
if "%~1"=="--output-schema" if defined MOCK_CODEX_SCHEMA copy /y "%~2" "%MOCK_CODEX_SCHEMA%" >nul
if "%~1"=="--output-last-message" set "LAST=%~2"
shift
goto :scan
:scanned
if defined MOCK_CODEX_NO_REPLY goto :done
if not defined LAST goto :done
if defined MOCK_CODEX_REPLY (copy /y "%MOCK_CODEX_REPLY%" "%LAST%" >nul) else (>"%LAST%" echo {"class":"normal"})
:done
if defined MOCK_CODEX_EXIT exit /b %MOCK_CODEX_EXIT%
exit /b 0
:version
echo 0.0.0 (mock)
if defined MOCK_CODEX_VERSION_EXIT exit /b %MOCK_CODEX_VERSION_EXIT%
exit /b 0
