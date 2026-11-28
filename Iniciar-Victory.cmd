@echo off
setlocal
if not exist "%~dp0Iniciar-Victory.exe" (
  echo Este checkout contem codigo-fonte, sem o iniciador compilado.
  echo Baixe o pacote Windows completo em:
  echo https://github.com/GiovannaSGuerrera/Victory-RTX2080/releases
  echo Extraia o ZIP inteiro e abra Iniciar-Victory.exe.
  pause
  exit /b 1
)
start "" "%~dp0Iniciar-Victory.exe"
