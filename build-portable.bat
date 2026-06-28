@echo off
title xxterm - Build Portable

setlocal enabledelayedexpansion

set "ROOT=%~dp0"
set "RELEASE=%ROOT%release"

echo Building xxterm portable executable...
echo.

:: ---------- Step 1: npm install ----------
echo [1/3] Installing frontend dependencies...
cd /d "%ROOT%"
call npm install
if %errorlevel% neq 0 (
    echo npm install failed. Aborting.
    exit /b 1
)
echo Done.
echo.

:: ---------- Step 2: Tauri build ----------
echo [2/3] Building Tauri application without installers (this may take a while)...
cd /d "%ROOT%"
call npx tauri build --no-bundle
if %errorlevel% neq 0 (
    echo Tauri build failed. Aborting.
    exit /b 1
)
echo Done.
echo.

:: ---------- Step 3: Copy executable ----------
echo [3/3] Copying portable executable...
if not exist "%RELEASE%" mkdir "%RELEASE%"

set "EXE_SRC=%ROOT%src-tauri\target\release\xxterm.exe"
if not exist "%EXE_SRC%" (
    echo ERROR: Built executable not found at %EXE_SRC%
    exit /b 1
)
copy "%EXE_SRC%" "%RELEASE%\xxterm.exe" >nul

echo Done.
echo.

echo ==============================================
echo  Portable executable: %RELEASE%\xxterm.exe
echo ==============================================
