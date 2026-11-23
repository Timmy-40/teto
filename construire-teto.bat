@echo off
chcp 65001 >nul
cd /d "%~dp0"
title Construction de Teto
set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"

if not exist "%CARGO%" (
  echo [1/3] Installation de Rust ^(une seule fois^)...
  winget install -e --id Rustlang.Rustup --accept-package-agreements --accept-source-agreements
  "%USERPROFILE%\.cargo\bin\rustup.exe" default stable-msvc
  echo.
  echo [2/3] Installation des outils de compilation Microsoft ^(une seule fois, peut etre long^)...
  winget install -e --id Microsoft.VisualStudio.2022.BuildTools --accept-package-agreements --accept-source-agreements --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
)

if not exist "%CARGO%" (
  echo.
  echo Rust n'a pas pu etre installe automatiquement. Installe-le depuis https://rustup.rs puis relance ce fichier.
  pause
  exit /b 1
)

echo.
echo [3/3] Construction de Teto ^(quelques minutes la premiere fois^)...
"%CARGO%" build --release
if errorlevel 1 (
  echo.
  echo La construction a echoue. Copie le message ci-dessus et envoie-le moi.
  pause
  exit /b 1
)

copy /y "target\release\teto.exe" "Teto.exe" >nul
echo.
echo Termine ! Teto.exe est dans ce dossier. Tu peux le copier ou tu veux : Rust n'est plus necessaire pour le lancer.
pause
