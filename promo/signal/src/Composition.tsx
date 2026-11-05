import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { AbsoluteFill, Composition, staticFile, useCurrentFrame, useVideoConfig } from "remotion";
import { Audio } from "@remotion/media";
import { fontFamily as monoFamily, loadFont as loadMono } from "@remotion/google-fonts/JetBrainsMono";
import { fontFamily as serifFamily, loadFont as loadSerif } from "@remotion/google-fonts/InstrumentSerif";
import TL from "./film/timeline.json";
import { H, makeSurfaces, renderFrame, Surfaces, W } from "./film/engine";

const mono = loadMono("normal", { weights: ["300", "400", "500", "600"], subsets: ["latin"] });
const serif = loadSerif("italic", { weights: ["400"], subsets: ["latin"] });
const fontsReady = Promise.all([mono.waitUntilDone(), serif.waitUntilDone()]);

export const Signal: React.FC<{ withMusic: boolean }> = ({ withMusic }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const ref = useRef<HTMLCanvasElement>(null);
  const surfaces = useRef<Surfaces | null>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    fontsReady.then(() => setReady(true));
  }, []);

  useLayoutEffect(() => {
    if (!ready || !ref.current) return;
    surfaces.current ??= makeSurfaces(ref.current.getContext("2d")!, document);
    renderFrame(surfaces.current, frame / fps, { mono: monoFamily, serif: serifFamily });
  }, [frame, fps, ready]);

  return (
    <AbsoluteFill style={{ backgroundColor: "#030508" }}>
      <canvas ref={ref} width={W} height={H} style={{ width: "100%", height: "100%" }} />
      {withMusic ? <Audio src={staticFile("score.wav")} /> : null}
    </AbsoluteFill>
  );
};

export const MyComposition = () => (
  <Composition
    id="Signal"
    component={Signal}
    durationInFrames={Math.round(TL.duration * TL.fps)}
    fps={TL.fps}
    width={W}
    height={H}
    defaultProps={{ withMusic: false }}
  />
);
