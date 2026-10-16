@echo off
:: ========================================================================
:: x360-rs Menu Helper
:: Quick-access wrapper for x360-rs.exe
:: Place this next to x360-rs.exe (target/release/) or in the project root.
:: Drag a file onto this bat to parse it directly.
:: ========================================================================

setlocal enabledelayedexpansion

:: Find the executable next to this script, else fall back to target/release
set "EXE=%~dp0x360-rs.exe"
if not exist "%EXE%" set "EXE=%~dp0target\release\x360-rs.exe"
if not exist "%EXE%" set "EXE=%~dp0..\target\release\x360-rs.exe"
if not exist "%EXE%" (
    echo ERROR: x360-rs.exe not found next to this script or in target/release/
    pause
    exit /b 1
)

:: If a file was dragged onto this bat, parse it directly
if not "%~1"=="" (
    "%EXE%" "%~1"
    pause
    exit /b 0
)

:MENU
cls
echo ================================================================
echo  x360-rs Menu Helper
echo ================================================================
echo.
echo  EXE: %EXE%
echo.
echo  1) Parse a single file (info / report)
echo  2) Parse a directory recursively (warns on unknown files)
echo  3) Verify STFS hash chain (--verify)
echo  4) Extract STFS contents (--extract)
echo  5) Extract STFS thumbnails (--extract-thumbnail)
echo  6) Extract XISO/GDF contents (--extract-iso)
echo  7) Optimize/repack XISO (--optimize)
echo  8) Truncate XISO trailing sectors (--truncate)
echo  9) MD5 checksums for XISO entries (--md5)
echo 10) Deterministic content checksum (--checksum)
echo 11) Show raw strings in a file (--show-strings)
echo 12) Run full test suite (cargo test --locked)
echo 13) Build release (cargo build --release --locked)
echo 14) Exit
echo.
set "CHOICE="
set /p "CHOICE=Choose an option (1-14): "
if not defined CHOICE goto MENU

if "!CHOICE!"=="1" goto PARSE_FILE
if "!CHOICE!"=="2" goto PARSE_DIR
if "!CHOICE!"=="3" goto VERIFY_STFS
if "!CHOICE!"=="4" goto EXTRACT_STFS
if "!CHOICE!"=="5" goto EXTRACT_THUMB
if "!CHOICE!"=="6" goto EXTRACT_XISO
if "!CHOICE!"=="7" goto OPTIMIZE_XISO
if "!CHOICE!"=="8" goto TRUNCATE_XISO
if "!CHOICE!"=="9" goto MD5_XISO
if "!CHOICE!"=="10" goto CHECKSUM_XISO
if "!CHOICE!"=="11" goto SHOW_STRINGS
if "!CHOICE!"=="12" goto RUN_TESTS
if "!CHOICE!"=="13" goto BUILD_RELEASE
if "!CHOICE!"=="14" goto END

goto MENU

:PAUSE_AND_MENU
pause
goto MENU

:PARSE_FILE
cls
echo --- Parse a single file ---
set "FILE="
set /p "FILE=Enter file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
"%EXE%" "!FILE!"
pause
goto MENU

:PARSE_DIR
cls
echo --- Parse directory recursively ---
set "DIR="
set /p "DIR=Enter directory path: "
if not defined DIR goto PAUSE_AND_MENU
set "DIR=!DIR:"=!"
"%EXE%" --dir "!DIR!" --recursive
pause
goto MENU

:VERIFY_STFS
cls
echo --- Verify STFS hash chain ---
set "FILE="
set /p "FILE=Enter STFS file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
"%EXE%" "!FILE!" --verify
pause
goto MENU

:EXTRACT_STFS
cls
echo --- Extract STFS contents ---
set "FILE="
set "OUT="
set /p "FILE=Enter STFS file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
set /p "OUT=Enter output directory: "
if not defined OUT goto PAUSE_AND_MENU
set "OUT=!OUT:"=!"
"%EXE%" "!FILE!" --extract "!OUT!"
pause
goto MENU

:EXTRACT_THUMB
cls
echo --- Extract STFS thumbnails ---
set "FILE="
set "OUT="
set /p "FILE=Enter STFS file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
set /p "OUT=Enter output directory: "
if not defined OUT goto PAUSE_AND_MENU
set "OUT=!OUT:"=!"
"%EXE%" "!FILE!" --extract-thumbnail "!OUT!"
pause
goto MENU

:EXTRACT_XISO
cls
echo --- Extract XISO/GDF contents ---
set "FILE="
set "OUT="
set /p "FILE=Enter XISO file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
set /p "OUT=Enter output directory: "
if not defined OUT goto PAUSE_AND_MENU
set "OUT=!OUT:"=!"
"%EXE%" "!FILE!" --extract-iso "!OUT!"
pause
goto MENU

:OPTIMIZE_XISO
cls
echo --- Optimize XISO ---
set "FILE="
set "OUT="
set /p "FILE=Enter XISO file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
set /p "OUT=Enter output file (or leave blank for in-place): "
if not defined OUT (
    "%EXE%" "!FILE!" --optimize --in-place --verify-output
) else (
    set "OUT=!OUT:"=!"
    "%EXE%" "!FILE!" --optimize -o "!OUT!" --verify-output
)
pause
goto MENU

:TRUNCATE_XISO
cls
echo --- Truncate XISO trailing sectors ---
set "FILE="
set "OUT="
set /p "FILE=Enter XISO file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
set /p "OUT=Enter output file: "
if not defined OUT goto PAUSE_AND_MENU
set "OUT=!OUT:"=!"
"%EXE%" "!FILE!" --truncate -o "!OUT!"
pause
goto MENU

:MD5_XISO
cls
echo --- MD5 checksums for XISO entries ---
set "FILE="
set /p "FILE=Enter XISO file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
"%EXE%" "!FILE!" --md5
pause
goto MENU

:CHECKSUM_XISO
cls
echo --- Deterministic content checksum ---
set "FILE="
set /p "FILE=Enter XISO file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
"%EXE%" "!FILE!" --checksum
pause
goto MENU

:SHOW_STRINGS
cls
echo --- Show raw strings in a file ---
set "FILE="
set /p "FILE=Enter file path: "
if not defined FILE goto PAUSE_AND_MENU
set "FILE=!FILE:"=!"
"%EXE%" "!FILE!" --show-strings
pause
goto MENU

:RUN_TESTS
cls
echo --- Running cargo test --locked ---
cargo test --locked
pause
goto MENU

:BUILD_RELEASE
cls
echo --- Building release ---
cargo build --release --locked
pause
goto MENU

:END
echo Goodbye.
pause
endlocal
exit /b 0