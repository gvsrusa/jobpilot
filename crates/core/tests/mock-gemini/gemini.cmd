@echo off
rem Stands in for the real `gemini` in tests. Never reaches a network. Each test's wrapper script
rem sets MOCK_GEMINI_*, because the adapter clears the child's environment.
rem Differences from the sh mock: no pid (cmd has no $$), and %* lands on one line of the args file.
setlocal
if "%~1"=="--version" goto :version
if defined MOCK_GEMINI_CWD cd >"%MOCK_GEMINI_CWD%"
if defined MOCK_GEMINI_CWD dir /a /b >>"%MOCK_GEMINI_CWD%" 2>nul
if not defined MOCK_GEMINI_SLEEP goto :record
set /a PINGS=%MOCK_GEMINI_SLEEP%+1
ping -n %PINGS% 127.0.0.1 >nul
if defined MOCK_GEMINI_DONE type nul >"%MOCK_GEMINI_DONE%"
:record
if defined MOCK_GEMINI_ARGS >"%MOCK_GEMINI_ARGS%" echo(%*
if defined MOCK_GEMINI_STDIN findstr "^" >"%MOCK_GEMINI_STDIN%"
if defined MOCK_GEMINI_ENV set >"%MOCK_GEMINI_ENV%"
:scan
if "%~1"=="" goto :scanned
if not "%~1"=="--policy" goto :next
if defined MOCK_GEMINI_POLICY copy /y "%~2" "%MOCK_GEMINI_POLICY%" >nul
:next
shift
goto :scan
:scanned
if defined MOCK_GEMINI_SYSTEM_COPY copy /y "%GEMINI_SYSTEM_MD%" "%MOCK_GEMINI_SYSTEM_COPY%" >nul
if defined MOCK_GEMINI_SETTINGS_COPY copy /y "%GEMINI_CLI_SYSTEM_SETTINGS_PATH%" "%MOCK_GEMINI_SETTINGS_COPY%" >nul
if defined MOCK_GEMINI_STDERR type "%MOCK_GEMINI_STDERR%" 1>&2
if defined MOCK_GEMINI_REPLY goto :reply
echo {"type":"init","session_id":"s","model":"m"}
echo {"type":"message","role":"user","content":"x"}
echo {"type":"message","role":"assistant","content":"{\"class\":","delta":true}
echo {"type":"message","role":"assistant","content":"\"normal\"}","delta":true}
echo {"type":"result","status":"success","stats":{}}
goto :done
:reply
type "%MOCK_GEMINI_REPLY%"
:done
if not defined MOCK_GEMINI_EXIT set MOCK_GEMINI_EXIT=0
exit /b %MOCK_GEMINI_EXIT%
:version
if defined MOCK_GEMINI_VERSION (echo %MOCK_GEMINI_VERSION%) else (echo 0.40.0)
if defined MOCK_GEMINI_VERSION_EXIT exit /b %MOCK_GEMINI_VERSION_EXIT%
exit /b 0
