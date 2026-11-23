import { useQuery } from "@tanstack/react-query";
import {
  ChevronDown,
  ChevronUp,
  Pause,
  Play,
  SkipBack,
  SkipForward,
  SlidersHorizontal,
  X,
} from "lucide-react";
import * as React from "react";
import { ttsAudioUrl } from "@/api/client";
import { featuresApi } from "@/api/endpoints";
import { cn } from "@/lib/cn";
import { usePrefsStore, type TtsEngine } from "@/stores/prefs";
import { notify } from "@/stores/toast";
import { Switch } from "@/ui/Controls";
import { AnimatePresence, motion, SPRING_POP, SPRING_SHEET, useReducedMotion } from "@/ui/motion";

export interface TtsPlayerBarProps {
  open: boolean;
  onClose: () => void;
  paragraphs: string[];
  chapterIndex: number;
  chapterTitle: string;
  activeParaIndex: number;
  onActiveParaChange: (index: number) => void;
  hasNextChapter: boolean;
  onRequestNextChapter: () => void;
  hudOpen: boolean;
  workbenchOpen: boolean;
}

const RATE_STEPS = [0.8, 1.0, 1.2, 1.4, 1.6, 1.8] as const;

const EDGE_VOICES = [
  { id: "zh-CN-XiaoxiaoNeural", label: "晓晓 · 温润女声" },
  { id: "zh-CN-YunxiNeural", label: "云希 · 朗润男声" },
  { id: "zh-CN-YunjianNeural", label: "云健 · 沉稳播讲" },
  { id: "zh-CN-XiaoyiNeural", label: "晓伊 · 清澈柔和" },
  { id: "zh-CN-liaoning-XiaobeiNeural", label: "晓北 · 叙事感" },
] as const;

function formatEdgeRate(rate: number): string {
  const pct = Math.round((rate - 1) * 100);
  return `${pct >= 0 ? "+" : ""}${pct}%`;
}

function formatHttpRate(rate: number): string {
  // Per backend contract: speed = (5 + (rate - 0.5) * 30)
  return (Math.max(0.1, rate * 0.5)).toFixed(2);
}

export function TtsPlayerBar({
  open,
  onClose,
  paragraphs,
  chapterIndex,
  chapterTitle,
  activeParaIndex,
  onActiveParaChange,
  hasNextChapter,
  onRequestNextChapter,
  hudOpen,
  workbenchOpen,
}: TtsPlayerBarProps) {
  const ttsEngine = usePrefsStore((s) => s.ttsEngine);
  const ttsVoice = usePrefsStore((s) => s.ttsVoice);
  const ttsRate = usePrefsStore((s) => s.ttsRate);
  const ttsAutoNext = usePrefsStore((s) => s.ttsAutoNext);
  const setTtsEngine = usePrefsStore((s) => s.setTtsEngine);
  const setTtsVoice = usePrefsStore((s) => s.setTtsVoice);
  const setTtsRate = usePrefsStore((s) => s.setTtsRate);
  const setTtsAutoNext = usePrefsStore((s) => s.setTtsAutoNext);

  const reduced = useReducedMotion();

  const [playing, setPlaying] = React.useState(false);
  const [buffering, setBuffering] = React.useState(false);
  const [expanded, setExpanded] = React.useState(false);
  const [showVoiceConfig, setShowVoiceConfig] = React.useState(false);
  const [browserVoices, setBrowserVoices] = React.useState<SpeechSynthesisVoice[]>([]);

  const audioRef = React.useRef<HTMLAudioElement | null>(null);
  const preloadAudioRef = React.useRef<{ key: string; el: HTMLAudioElement } | null>(null);
  const epochRef = React.useRef(0);
  const awaitingAutoNextRef = React.useRef(false);
  const prevChapterRef = React.useRef(chapterIndex);
  const paraIndexRef = React.useRef(activeParaIndex);
  paraIndexRef.current = activeParaIndex;
  const paragraphsRef = React.useRef(paragraphs);
  paragraphsRef.current = paragraphs;

  const { data: httpTtsList = [] } = useQuery({
    queryKey: ["http-tts-list"],
    queryFn: () => featuresApi.listHttpTts(),
    enabled: open,
    staleTime: 60_000,
  });

  // Load browser speechSynthesis voices
  React.useEffect(() => {
    if (!open || typeof window === "undefined" || !("speechSynthesis" in window)) return;
    const sync = () => {
      const all = window.speechSynthesis.getVoices();
      const sorted = [...all].sort((a, b) => {
        const aZh = a.lang.toLowerCase().startsWith("zh") ? 0 : 1;
        const bZh = b.lang.toLowerCase().startsWith("zh") ? 0 : 1;
        return aZh - bZh || a.name.localeCompare(b.name);
      });
      setBrowserVoices(sorted);
    };
    sync();
    window.speechSynthesis.addEventListener?.("voiceschanged", sync);
    return () => {
      window.speechSynthesis.removeEventListener?.("voiceschanged", sync);
    };
  }, [open]);

  const stopPlayback = React.useCallback(() => {
    epochRef.current += 1;
    if (typeof window !== "undefined" && "speechSynthesis" in window) {
      try {
        window.speechSynthesis.cancel();
      } catch {
        // ignore
      }
    }
    if (audioRef.current) {
      audioRef.current.onended = null;
      audioRef.current.onerror = null;
      audioRef.current.oncanplay = null;
      audioRef.current.pause();
      audioRef.current.removeAttribute("src");
      audioRef.current.load();
    }
    setBuffering(false);
  }, []);

  // Build backend TTS URL for a paragraph
  const buildBackendUrl = React.useCallback(
    (text: string): string | null => {
      if (ttsEngine === "edge") {
        const voice =
          EDGE_VOICES.some((v) => v.id === ttsVoice) ? ttsVoice : EDGE_VOICES[0].id;
        return ttsAudioUrl({
          text,
          type: "edge",
          voice,
          rate: formatEdgeRate(ttsRate),
        });
      }
      if (ttsEngine === "http") {
        const chosen =
          httpTtsList.find((item) => item.name === ttsVoice) ?? httpTtsList[0];
        if (!chosen) return null;
        return ttsAudioUrl({
          text,
          type: "http",
          voice: chosen.name,
          rate: formatHttpRate(ttsRate),
        });
      }
      return null;
    },
    [httpTtsList, ttsEngine, ttsRate, ttsVoice],
  );

  // Speak a specific paragraph index
  const speakParagraph = React.useCallback(
    (index: number) => {
      stopPlayback();
      const myEpoch = epochRef.current;

      // Skip empty paragraphs automatically
      let nextIdx = index;
      while (nextIdx < paragraphs.length && !paragraphs[nextIdx]?.trim()) {
        nextIdx += 1;
      }

      if (nextIdx >= paragraphs.length) {
        if (ttsAutoNext && hasNextChapter) {
          awaitingAutoNextRef.current = true;
          setBuffering(true);
          onRequestNextChapter();
        } else {
          setPlaying(false);
          notify.info("本章朗读完成");
        }
        return;
      }

      if (nextIdx !== paraIndexRef.current) {
        onActiveParaChange(nextIdx);
        return;
      }

      const text = paragraphs[nextIdx]!.trim();

      const advance = () => {
        if (epochRef.current !== myEpoch) return;
        let target = nextIdx + 1;
        while (target < paragraphs.length && !paragraphs[target]?.trim()) {
          target += 1;
        }
        if (target < paragraphs.length) {
          onActiveParaChange(target);
        } else if (ttsAutoNext && hasNextChapter) {
          awaitingAutoNextRef.current = true;
          setBuffering(true);
          onRequestNextChapter();
        } else {
          setPlaying(false);
          notify.info("本章朗读完成");
        }
      };

      if (ttsEngine === "browser") {
        if (typeof window === "undefined" || !("speechSynthesis" in window)) {
          notify.error("当前浏览器不支持本地语音朗读，请切换至 Edge 云端语音");
          setPlaying(false);
          return;
        }
        const utter = new SpeechSynthesisUtterance(text);
        utter.lang = "zh-CN";
        utter.rate = ttsRate;
        const chosenVoice =
          browserVoices.find((v) => v.voiceURI === ttsVoice || v.name === ttsVoice) ??
          browserVoices.find((v) => v.lang.toLowerCase().startsWith("zh"));
        if (chosenVoice) utter.voice = chosenVoice;

        utter.onend = () => advance();
        utter.onerror = (ev) => {
          if (epochRef.current !== myEpoch) return;
          if (ev.error === "canceled" || ev.error === "interrupted") return;
          setPlaying(false);
          notify.error("浏览器语音朗读中断，可尝试切换至 Edge 语音");
        };
        window.speechSynthesis.speak(utter);
        return;
      }

      // Backend TTS (Edge or custom HttpTTS)
      const url = buildBackendUrl(text);
      if (!url) {
        notify.error("尚未配置可用的自定义听书源，请在设置中添加或切换 Edge 语音");
        setPlaying(false);
        return;
      }

      setBuffering(true);

      // Reuse preloaded audio element if it matches this URL
      let audio: HTMLAudioElement;
      if (preloadAudioRef.current?.key === url) {
        audio = preloadAudioRef.current.el;
        preloadAudioRef.current = null;
      } else {
        audio = new Audio(url);
      }
      audioRef.current = audio;

      audio.oncanplay = () => {
        if (epochRef.current !== myEpoch) return;
        setBuffering(false);
      };
      audio.onended = () => {
        advance();
      };
      audio.onerror = () => {
        if (epochRef.current !== myEpoch) return;
        setBuffering(false);
        setPlaying(false);
        notify.error("云端语音合成失败，请检查网络或切换语音引擎");
      };

      void audio.play().catch((err) => {
        if (epochRef.current !== myEpoch) return;
        setBuffering(false);
        setPlaying(false);
        if (err instanceof DOMException && err.name === "NotAllowedError") {
          notify.info("请点击播放按钮开始听书");
        } else {
          notify.error("语音播放失败");
        }
      });

      // Quietly preload next paragraph
      const lookaheadText = paragraphs[nextIdx + 1]?.trim();
      if (lookaheadText) {
        const nextUrl = buildBackendUrl(lookaheadText);
        if (nextUrl) {
          const pre = new Audio();
          pre.preload = "auto";
          pre.src = nextUrl;
          preloadAudioRef.current = { key: nextUrl, el: pre };
        }
      }
    },
    [
      browserVoices,
      buildBackendUrl,
      hasNextChapter,
      onActiveParaChange,
      onRequestNextChapter,
      paragraphs,
      stopPlayback,
      ttsAutoNext,
      ttsEngine,
      ttsRate,
      ttsVoice,
    ],
  );

  // Start speaking automatically when bar opens
  React.useEffect(() => {
    if (!open) {
      stopPlayback();
      setPlaying(false);
      awaitingAutoNextRef.current = false;
      return;
    }
    if (paragraphsRef.current.length > 0) {
      setPlaying(true);
    }
  }, [open, stopPlayback]);

  // Trigger playback whenever playing state / activeParaIndex / engine settings change
  React.useEffect(() => {
    if (!open || !playing) {
      stopPlayback();
      return;
    }
    if (paragraphs.length === 0) return;

    if (prevChapterRef.current !== chapterIndex) {
      prevChapterRef.current = chapterIndex;
      if (awaitingAutoNextRef.current) {
        awaitingAutoNextRef.current = false;
        onActiveParaChange(0);
        speakParagraph(0);
        return;
      }
    }

    speakParagraph(Math.min(activeParaIndex, paragraphs.length - 1));
    return () => {
      stopPlayback();
    };
    // We intentionally restart the active utterance when the target paragraph or voice parameters change.
  }, [
    open,
    playing,
    activeParaIndex,
    chapterIndex,
    paragraphs,
    ttsEngine,
    ttsVoice,
    ttsRate,
    speakParagraph,
    stopPlayback,
    onActiveParaChange,
  ]);

  // Ensure clean unmount
  React.useEffect(() => () => stopPlayback(), [stopPlayback]);

  const handleCycleRate = () => {
    const idx = RATE_STEPS.findIndex((r) => Math.abs(r - ttsRate) < 0.05);
    const next = RATE_STEPS[(idx + 1) % RATE_STEPS.length] ?? 1.0;
    setTtsRate(next);
  };

  const handleStep = (delta: number) => {
    if (paragraphs.length === 0) return;
    const next = Math.min(paragraphs.length - 1, Math.max(0, activeParaIndex + delta));
    onActiveParaChange(next);
    if (!playing) setPlaying(true);
  };

  const currentSnippet = paragraphs[activeParaIndex]?.trim() || chapterTitle || "准备朗读…";

  const activeVoiceLabel = React.useMemo(() => {
    if (ttsEngine === "edge") {
      return EDGE_VOICES.find((v) => v.id === ttsVoice)?.label ?? EDGE_VOICES[0].label;
    }
    if (ttsEngine === "http") {
      return httpTtsList.find((v) => v.name === ttsVoice)?.name ?? httpTtsList[0]?.name ?? "HttpTTS";
    }
    const bv =
      browserVoices.find((v) => v.voiceURI === ttsVoice || v.name === ttsVoice) ??
      browserVoices.find((v) => v.lang.toLowerCase().startsWith("zh"));
    return bv ? bv.name : "系统语音";
  }, [browserVoices, httpTtsList, ttsEngine, ttsVoice]);

  // Dynamic Island sits at top-center; gracefully clears mobile top-left return capsule when HUD is open
  const topPositionClass = hudOpen
    ? "top-[64px] md:top-4"
    : "top-3 sm:top-4";

  const waveform = (
    <div
      aria-hidden
      className="flex h-5 w-7 shrink-0 items-end justify-center gap-[2.5px] px-0.5"
    >
      {buffering ? (
        <span className="size-3.5 animate-spin rounded-full border-2 border-emerald-400/30 border-t-emerald-400" />
      ) : (
        [0, 140, 290, 90, 220].map((delay, idx) => (
          <span
            key={idx}
            data-paused={!playing ? "true" : "false"}
            style={{ animationDelay: `${delay}ms` }}
            className="tts-eq-bar bg-emerald-400"
          />
        ))
      )}
    </div>
  );

  return (
    <AnimatePresence>
      {open ? (
        <motion.div
          key="tts-dynamic-island"
          layout={!reduced}
          initial={reduced ? { opacity: 0 } : { opacity: 0, y: -18, scale: 0.92 }}
          animate={{ opacity: workbenchOpen ? 0.96 : 1, y: 0, scale: 1 }}
          exit={reduced ? { opacity: 0 } : { opacity: 0, y: -16, scale: 0.92 }}
          transition={reduced ? { duration: 0.14 } : SPRING_POP}
          onClick={(e) => e.stopPropagation()}
          className={cn(
            "fixed left-1/2 z-35 -translate-x-1/2 safe-t transition-[top] duration-300 ease-[cubic-bezier(0.22,1,0.36,1)]",
            topPositionClass,
            expanded ? "w-[min(94vw,438px)]" : "w-[min(92vw,324px)]",
          )}
        >
          <motion.div
            layout={!reduced}
            transition={reduced ? { duration: 0.14 } : SPRING_SHEET}
            className={cn(
              "overflow-hidden border border-white/12 bg-[#161412]/94 text-[#F7F3EB] shadow-[0_22px_48px_-12px_rgba(12,10,8,0.52),0_4px_14px_rgba(0,0,0,0.24)] backdrop-blur-2xl",
              expanded ? "rounded-[26px]" : "rounded-full",
            )}
          >
            {/* Compact Living Waveform Header */}
            <div
              className={cn(
                "flex items-center gap-2",
                expanded ? "px-4 pt-3.5 pb-2.5" : "py-1.5 pr-2 pl-3",
              )}
            >
              <button
                type="button"
                aria-label={expanded ? "收起听书灵动岛" : "展开听书灵动岛"}
                onClick={() => setExpanded((v) => !v)}
                className="flex min-w-0 flex-1 cursor-pointer items-center gap-2.5 text-left outline-none"
              >
                {waveform}
                <div className="min-w-0 flex-1">
                  <p className="truncate font-serif text-xs font-medium text-[#F7F3EB]">
                    {currentSnippet}
                  </p>
                  <p className="mt-0.5 flex items-center gap-1.5 text-[10px] tabular-nums text-white/55">
                    <span>
                      第 {Math.min(activeParaIndex + 1, Math.max(1, paragraphs.length))} /{" "}
                      {Math.max(1, paragraphs.length)} 段
                    </span>
                    <span>·</span>
                    <span className="truncate">{activeVoiceLabel}</span>
                  </p>
                </div>
              </button>

              {!expanded ? (
                <button
                  type="button"
                  aria-label={playing ? "暂停朗读" : "继续朗读"}
                  onClick={() => setPlaying((p) => !p)}
                  className="flex size-7.5 shrink-0 cursor-pointer items-center justify-center rounded-full bg-emerald-500 text-[#0c1a14] shadow-sm transition-transform hover:brightness-110 active:scale-95"
                >
                  {playing ? (
                    <Pause className="size-3.5 fill-current" />
                  ) : (
                    <Play className="size-3.5 translate-x-[0.5px] fill-current" />
                  )}
                </button>
              ) : null}

              <button
                type="button"
                aria-label={expanded ? "收起听书面板" : "展开听书面板"}
                onClick={() => setExpanded((v) => !v)}
                className="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-full text-white/65 transition-colors hover:bg-white/10 hover:text-white"
              >
                {expanded ? (
                  <ChevronUp className="size-3.5" />
                ) : (
                  <ChevronDown className="size-3.5" />
                )}
              </button>

              <button
                type="button"
                aria-label="退出听书"
                onClick={() => {
                  stopPlayback();
                  setPlaying(false);
                  setExpanded(false);
                  onClose();
                }}
                className="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-full text-white/60 transition-colors hover:bg-white/10 hover:text-white"
              >
                <X className="size-3.5" />
              </button>
            </div>

            {/* Expanded Spring Morphing Deck */}
            <AnimatePresence initial={false}>
              {expanded ? (
                <motion.div
                  key="tts-expanded-deck"
                  initial={reduced ? { opacity: 0 } : { opacity: 0, height: 0 }}
                  animate={{ opacity: 1, height: "auto" }}
                  exit={reduced ? { opacity: 0 } : { opacity: 0, height: 0 }}
                  transition={reduced ? { duration: 0.14 } : SPRING_SHEET}
                  className="overflow-hidden border-t border-white/10"
                >
                  {/* Transport & Speed Bar */}
                  <div className="flex items-center justify-between gap-2 px-4 py-3">
                    <button
                      type="button"
                      onClick={handleCycleRate}
                      title="切换语速"
                      className="h-8 shrink-0 cursor-pointer rounded-full bg-white/10 px-3 font-mono text-[11px] font-medium tabular-nums text-emerald-300 transition-colors hover:bg-white/16"
                    >
                      {ttsRate.toFixed(1)}x
                    </button>

                    <div className="flex items-center gap-2">
                      <button
                        type="button"
                        aria-label="上一段"
                        disabled={activeParaIndex <= 0}
                        onClick={() => handleStep(-1)}
                        className="flex size-8 cursor-pointer items-center justify-center rounded-full bg-white/8 text-white/85 transition-colors hover:bg-white/16 disabled:pointer-events-none disabled:opacity-35"
                      >
                        <SkipBack className="size-3.5" />
                      </button>

                      <button
                        type="button"
                        aria-label={playing ? "暂停朗读" : "继续朗读"}
                        onClick={() => setPlaying((p) => !p)}
                        className="flex h-9 w-16 cursor-pointer items-center justify-center rounded-full bg-emerald-500 text-[#0c1a14] shadow-md transition-transform hover:brightness-110 active:scale-95"
                      >
                        {playing ? (
                          <Pause className="size-4 fill-current" />
                        ) : (
                          <Play className="size-4 translate-x-[0.5px] fill-current" />
                        )}
                      </button>

                      <button
                        type="button"
                        aria-label="下一段"
                        disabled={activeParaIndex >= paragraphs.length - 1 && !hasNextChapter}
                        onClick={() => handleStep(1)}
                        className="flex size-8 cursor-pointer items-center justify-center rounded-full bg-white/8 text-white/85 transition-colors hover:bg-white/16 disabled:pointer-events-none disabled:opacity-35"
                      >
                        <SkipForward className="size-3.5" />
                      </button>
                    </div>

                    <button
                      type="button"
                      aria-label="语音引擎与音色设置"
                      onClick={() => setShowVoiceConfig((v) => !v)}
                      className={cn(
                        "inline-flex h-8 shrink-0 cursor-pointer items-center gap-1.5 rounded-full px-3 text-[11px] font-medium transition-colors",
                        showVoiceConfig
                          ? "bg-emerald-500/20 text-emerald-300"
                          : "bg-white/10 text-white/80 hover:bg-white/16",
                      )}
                    >
                      <SlidersHorizontal className="size-3.5" />
                      <span>音色</span>
                    </button>
                  </div>

                  {/* Inline Voice & Engine Config Drawer */}
                  <AnimatePresence initial={false}>
                    {showVoiceConfig ? (
                      <motion.div
                        key="tts-voice-config"
                        initial={reduced ? { opacity: 0 } : { opacity: 0, height: 0 }}
                        animate={{ opacity: 1, height: "auto" }}
                        exit={reduced ? { opacity: 0 } : { opacity: 0, height: 0 }}
                        transition={reduced ? { duration: 0.12 } : SPRING_SHEET}
                        className="overflow-hidden border-t border-white/10 bg-black/20"
                      >
                        <div className="space-y-3 px-4 py-3.5">
                          {/* Engine Tabs */}
                          <div className="grid grid-cols-3 gap-1 rounded-full bg-white/8 p-1">
                            {(
                              [
                                { id: "browser", label: "浏览器" },
                                { id: "edge", label: "Edge 云端" },
                                { id: "http", label: "HttpTTS" },
                              ] as { id: TtsEngine; label: string }[]
                            ).map((eng) => (
                              <button
                                key={eng.id}
                                type="button"
                                onClick={() => setTtsEngine(eng.id)}
                                className={cn(
                                  "cursor-pointer rounded-full py-1 text-[11px] font-medium transition-colors",
                                  ttsEngine === eng.id
                                    ? "bg-emerald-500 text-[#0c1a14]"
                                    : "text-white/70 hover:text-white",
                                )}
                              >
                                {eng.label}
                              </button>
                            ))}
                          </div>

                          {/* Voices List */}
                          <div className="max-h-32 space-y-1 overflow-y-auto pr-0.5">
                            {ttsEngine === "edge" ? (
                              EDGE_VOICES.map((v) => {
                                const active =
                                  ttsVoice === v.id ||
                                  (!EDGE_VOICES.some((x) => x.id === ttsVoice) &&
                                    v.id === EDGE_VOICES[0].id);
                                return (
                                  <button
                                    key={v.id}
                                    type="button"
                                    onClick={() => setTtsVoice(v.id)}
                                    className={cn(
                                      "flex w-full cursor-pointer items-center justify-between rounded-xl px-3 py-1.5 text-left text-xs transition-colors",
                                      active
                                        ? "bg-emerald-500/20 font-medium text-emerald-300"
                                        : "text-white/75 hover:bg-white/8 hover:text-white",
                                    )}
                                  >
                                    <span>{v.label}</span>
                                  </button>
                                );
                              })
                            ) : ttsEngine === "http" ? (
                              httpTtsList.length === 0 ? (
                                <p className="rounded-xl bg-white/6 px-3 py-2 text-[11px] text-white/55">
                                  暂无自定义 HttpTTS 音源，可前往「设置 → 听书语音」添加。
                                </p>
                              ) : (
                                httpTtsList.map((item) => {
                                  const active =
                                    ttsVoice === item.name ||
                                    (!httpTtsList.some((x) => x.name === ttsVoice) &&
                                      item.name === httpTtsList[0]?.name);
                                  return (
                                    <button
                                      key={item.url}
                                      type="button"
                                      onClick={() => setTtsVoice(item.name)}
                                      className={cn(
                                        "flex w-full cursor-pointer items-center justify-between rounded-xl px-3 py-1.5 text-left text-xs transition-colors",
                                        active
                                          ? "bg-emerald-500/20 font-medium text-emerald-300"
                                          : "text-white/75 hover:bg-white/8 hover:text-white",
                                      )}
                                    >
                                      <span className="truncate">{item.name}</span>
                                    </button>
                                  );
                                })
                              )
                            ) : browserVoices.length === 0 ? (
                              <p className="rounded-xl bg-white/6 px-3 py-2 text-[11px] text-white/55">
                                使用系统默认中文语音
                              </p>
                            ) : (
                              browserVoices.slice(0, 12).map((v) => {
                                const active = ttsVoice === v.voiceURI || ttsVoice === v.name;
                                return (
                                  <button
                                    key={v.voiceURI}
                                    type="button"
                                    onClick={() => setTtsVoice(v.voiceURI)}
                                    className={cn(
                                      "flex w-full cursor-pointer items-center justify-between gap-2 rounded-xl px-3 py-1.5 text-left text-xs transition-colors",
                                      active
                                        ? "bg-emerald-500/20 font-medium text-emerald-300"
                                        : "text-white/75 hover:bg-white/8 hover:text-white",
                                    )}
                                  >
                                    <span className="truncate">{v.name}</span>
                                    <span className="shrink-0 text-[10px] text-white/45">
                                      {v.lang}
                                    </span>
                                  </button>
                                );
                              })
                            )}
                          </div>

                          {/* Auto-next toggle */}
                          <div className="flex items-center justify-between border-t border-white/10 pt-2.5">
                            <span className="text-xs text-white/75">读完自动连播下一章</span>
                            <Switch
                              checked={ttsAutoNext}
                              onChange={setTtsAutoNext}
                              ariaLabel="读完自动连播下一章"
                            />
                          </div>
                        </div>
                      </motion.div>
                    ) : null}
                  </AnimatePresence>
                </motion.div>
              ) : null}
            </AnimatePresence>
          </motion.div>
        </motion.div>
      ) : null}
    </AnimatePresence>
  );
}
