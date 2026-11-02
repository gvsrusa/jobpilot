@echo off
rem Stands in for the real `codex` in tests. Never reaches a network.
setlocal
if "%~1"=="--version" goto :version
if defined MOCK_CODEX_ARGS >"%MOCK_CODEX_ARGS%" echo(%*
if defined MOCK_CODEX_STDIN findstr "^" >"%MOCK_CODEX_STDIN%"
if defined MOCK_CODEX_EXIT exit /b %MOCK_CODEX_EXIT%
exit /b 0
:version
echo 0.0.0 (mock)
if defined MOCK_CODEX_VERSION_EXIT exit /b %MOCK_CODEX_VERSION_EXIT%
exit /b 0
