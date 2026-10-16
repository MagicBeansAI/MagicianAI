# Prologue reel source clips (NOT shipped)
Generated via Higgsfield Seedance 2.0, chained by start_image.
Frames under reel/ are the shipped asset; these mp4s are the masters.

Ten beats, in reel order:
beat1 bang · beat2 earth · beat3 dive · beat4a life-a · beat4b life-b ·
beat5 fire · beat6 stone · beat7 industry · beat8 desk · beat9 screen

`beat4-superseded-ape.mp4` is the retired single evolution beat: it hard-cut
from the dive and put a furry hominid in the tide pool, skipping the fish and
the tetrapod entirely. It is kept only as the before-picture.

The split beats pin BOTH endpoints where a downstream beat must survive:
beat4b carries `--end-image` = beat5's first frame, so beats 5-9 needed no
regeneration when the evolution grew from 5s to 10s.

Re-split recipe (reproduces the shipped frames byte-for-byte — the `lanczos`
flag is load-bearing, the default bicubic scaler does not match):

    ffmpeg -i src/beatN.mp4 \
      -vf "select=not(mod(n\,2)),scale=960:540:flags=lanczos" \
      -vsync 0 out_%03d.png
    cwebp -quiet -q 65 out_NNN.png -o fNNN.webp

Each beat is 5s at 24fps = 121 source frames; every 2nd frame gives 61 shipped
frames at 12fps. Ten beats = 610 contiguous frames. The LAST frame must stay
the match-cut CRT — PrologueReel maps scroll to frame linearly and MovieTrack's
DOM machine takes over from exactly that image.
