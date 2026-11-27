# Video and frames

`ddc-app` renders images without showing a window, so a model, the interface or the build-up of
a model can be turned into frames and then into a video. `ddc-app --help` lists the same flags.

## A still of the model

```sh
ddc-app --screenshot plan.png --open-project house.ddc --view plan --size 1920x1080
ddc-app --screenshot view.png --open-project house.ddc --view 3d
ddc-app --screenshot cut.png  --open-project house.ddc --view section:1
ddc-app --screenshot south.png --open-project house.ddc --view elevation:S
```

`--view` takes `plan`, `3d`, `section:N` (the N-th section, from 1) or `elevation:S` (N, S, E, W
or a number). `--scale F` scales the interface, `--lang CODE` sets its language and
`--theme light|dark` its theme. To work on a plain file instead of a project, see
`--file` in `ddc-app --help`.

## The interface

`--ui-shot script.txt` renders the whole editor offscreen, driven by a script. One step per
line, `#` starts a comment:

```
size 1600x1000
theme dark
open house.ddc
fit
shot plan.png
run arch.wall
shot wall-tool.png
```

The full list of steps (`tap`, `type`, `key`, `settings`, `wait` and others) is at the top of
`crates/app/src/ui_shot.rs`.

## Build-up animation

```sh
ddc-app --open-project house.ddc   --reveal all --reveal-out frames --reveal-fps 30 --size 1920x1080
```

The elements appear in build order from the bottom up, one PNG per frame:
`frames/frame_0000.png`, `frames/frame_0001.png` and so on. Every element of the 3D view takes
part, parts made by a script included. `--reveal calendar` follows the phase dates instead.
`--reveal-frame N` writes only that frame, `--reveal-motion full|reduced|off` sets how much the
elements move while they appear. `--reveal-out` renders without a window.

The camera frames the whole model and stays still (`--reveal-camera fixed`, the default).
`--reveal-camera orbit` turns once around the model over the show, and `--reveal-camera follow`
frames each step as it appears.

## Frames to video

Give `--reveal-out` a video file instead of a folder (`.mp4`, `.webm`, `.mov` or `.gif`) and
the frames are encoded with ffmpeg when it is on the PATH:

```sh
ddc-app --open-project house.ddc --reveal all --reveal-out build-up.mp4 --reveal-camera orbit
```

Without ffmpeg the frames stay in `build-up.mp4.frames` and the command to encode them is
printed; the run exits with code 2. Any encoder works on the numbered frames, for example:

```sh
ffmpeg -framerate 30 -i frames/frame_%04d.png -pix_fmt yuv420p build-up.mp4
```
