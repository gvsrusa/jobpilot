@echo off
rem Stands in for the real `gemini` in tests. Never reaches a network.
setlocal
if "%~1"=="--version" goto :version
if defined MOCK_GEMINI_ARGS >"%MOCK_GEMINI_ARGS%" echo(%*
if defined MOCK_GEMINI_STDIN findstr "^" >"%MOCK_GEMINI_STDIN%"
if defined MOCK_GEMINI_EXIT exit /b %MOCK_GEMINI_EXIT%
exit /b 0
:version
echo 0.0.0 (mock)
if defined MOCK_GEMINI_VERSION_EXIT exit /b %MOCK_GEMINI_VERSION_EXIT%
exit /b 0
