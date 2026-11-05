#!/usr/bin/env sh
# Regera os ícones a partir dos SVGs. Requer rsvg-convert (librsvg) e ImageMagick.
#   logo.svg       -> 48 px ou mais (com a seta de envio no centro)
#   logo-small.svg -> 16, 24 e 32 px (traço mais grosso, sem detalhes finos)
set -eu
cd "$(dirname "$0")"

for size in 16 24 32; do
    rsvg-convert -w "$size" -h "$size" logo-small.svg -o "icons/${size}x${size}.png"
done
for size in 48 64 96 128 256 512; do
    rsvg-convert -w "$size" -h "$size" logo.svg -o "icons/${size}x${size}.png"
done
rsvg-convert -w 1024 -h 1024 logo.svg -o icon.png

# Ícone do Windows com todos os tamanhos usados pelo Explorer e pela barra de tarefas.
magick icons/16x16.png icons/24x24.png icons/32x32.png icons/48x48.png \
    icons/64x64.png icons/128x128.png icons/256x256.png icon.ico
