import { createContext, memo, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import type { MediaItem, MediaSource, MediaStream } from "../../lib/api/types";
import { LuxSelect } from "../../components/LuxSelect";
import { defaultStreamIndex, streamsOfType, versionSelectableSources, sourceLabel, streamLabel } from "./media-source";

type SourceSelection = {
  sources: MediaSource[];
  source?: MediaSource;
  selectedAudioStreamIndex?: string;
  selectedSubtitleStreamIndex?: string;
  onSelect: (id: string) => void;
  onAudioSelect: (index: string) => void;
  onSubtitleSelect: (index: string) => void;
};
const SourceContext = createContext<SourceSelection | undefined>(undefined);
const emptySources: MediaSource[] = [];

export function MediaSourceProvider({ item, requestedSourceId, children }: { item: MediaItem; requestedSourceId?: string; children: ReactNode }) {
  const [selectedSourceId, setSelectedSourceId] = useState(requestedSourceId);
  const sources = item.mediaSources ?? emptySources;
  const source = sources.find((entry) => entry.id === selectedSourceId)
    ?? sources.find((entry) => entry.id === requestedSourceId)
    ?? sources.find((entry) => entry.isDefault)
    ?? sources[0];
  const [selectedAudioStreamIndex, setSelectedAudioStreamIndex] = useState(() => defaultStreamIndex(source, "AUDIO"));
  const [selectedSubtitleStreamIndex, setSelectedSubtitleStreamIndex] = useState(() => defaultStreamIndex(source, "SUBTITLE"));
  useEffect(() => { setSelectedSourceId(requestedSourceId); }, [requestedSourceId]);
  useEffect(() => {
    setSelectedAudioStreamIndex(defaultStreamIndex(source, "AUDIO"));
    setSelectedSubtitleStreamIndex(defaultStreamIndex(source, "SUBTITLE"));
  }, [source?.id]);
  const value = useMemo(() => ({
    sources, source, selectedAudioStreamIndex, selectedSubtitleStreamIndex,
    onSelect: setSelectedSourceId, onAudioSelect: setSelectedAudioStreamIndex, onSubtitleSelect: setSelectedSubtitleStreamIndex,
  }), [sources, source, selectedAudioStreamIndex, selectedSubtitleStreamIndex]);
  return <SourceContext value={value}>{children}</SourceContext>;
}

export function useMediaSource() {
  const value = useContext(SourceContext);
  if (!value) throw new Error("Media source selection requires MediaSourceProvider");
  return value;
}

export const MediaSourcePanel = memo(function MediaSourcePanel() {
  const selection = useMediaSource();
  const { sources, source } = selection;
  if (sources.length <= 1 && !streamsOfType(source, "AUDIO").length && !streamsOfType(source, "SUBTITLE").length) return null;
  return <MediaSourceSelector {...selection} selectedSourceId={source?.id} />;
});

function MediaSourceSelector({
  sources,
  selectedSourceId,
  selectedAudioStreamIndex,
  selectedSubtitleStreamIndex,
  onSelect,
  onAudioSelect,
  onSubtitleSelect,
}: {
  sources: MediaSource[];
  selectedSourceId?: string;
  selectedAudioStreamIndex?: string;
  selectedSubtitleStreamIndex?: string;
  onSelect: (sourceId: string) => void;
  onAudioSelect: (streamIndex: string) => void;
  onSubtitleSelect: (streamIndex: string) => void;
}) {
  const selectedSource = sources.find((source) => source.id === selectedSourceId) ?? sources[0];
  const audioStreams = streamsOfType(selectedSource, "AUDIO");
  const subtitleStreams = streamsOfType(selectedSource, "SUBTITLE");
  const versionSources = versionSelectableSources(sources);
  const options = versionSources.map((source, index) => ({
    value: source.id,
    label: (
      <span className="lux-source-option-content">
        <span className="lux-source-option-label">{sourceLabel(source, index)}</span>
        <span className="lux-source-option-detail">{source.editionName || source.container || "DIRECT PLAY"}</span>
      </span>
    ),
  }));

  return (
    <section
      className="lux-source-selector"
      aria-labelledby={versionSources.length > 1 ? "media-source-heading" : undefined}
      aria-label={versionSources.length > 1 ? undefined : "播放轨道选择"}
    >
      {versionSources.length > 1 ? <div className="lux-track-selector">
        <div className="lux-section-heading">
          <h2 id="media-source-heading">选择版本</h2>
          <span>{versionSources.length} 个版本</span>
        </div>
        <div className="lux-source-select lux-track-select">
          <LuxSelect
            value={selectedSourceId ?? sources[0]?.id ?? ""}
            options={options}
            menuClassName="lux-track-select-menu"
            onChange={onSelect}
            aria-labelledby="media-source-heading"
          />
        </div>
      </div> : null}
      {audioStreams.length || subtitleStreams.length ? (
        <div className="lux-track-selectors">
          {audioStreams.length ? (
            <MediaTrackSelector
              type="AUDIO"
              streams={audioStreams}
              selectedStreamIndex={selectedAudioStreamIndex}
              onSelect={onAudioSelect}
            />
          ) : null}
          {subtitleStreams.length ? (
            <MediaTrackSelector
              type="SUBTITLE"
              streams={subtitleStreams}
              selectedStreamIndex={selectedSubtitleStreamIndex}
              onSelect={onSubtitleSelect}
            />
          ) : null}
        </div>
      ) : null}
    </section>
  );
}

function MediaTrackSelector({
  type,
  streams,
  selectedStreamIndex,
  onSelect,
}: {
  type: "AUDIO" | "SUBTITLE";
  streams: MediaStream[];
  selectedStreamIndex?: string;
  onSelect: (streamIndex: string) => void;
}) {
  const label = type === "AUDIO" ? "音频" : "字幕";
  const headingId = `media-${type.toLowerCase()}-heading`;
  const options = streams.map((stream, index) => ({
    value: String(stream.index),
    label: streamLabel(stream, label, index),
  }));
  return (
    <div className="lux-track-selector" data-track-selector={type.toLowerCase()}>
      <div className="lux-section-heading">
        <h2 id={headingId}>选择{label}</h2>
        <span>{streams.length} 条{label}轨</span>
      </div>
      <div className="lux-track-select">
        <LuxSelect
          id={`media-${type.toLowerCase()}-select`}
          value={selectedStreamIndex ?? String(streams[0]?.index ?? "")}
          options={options}
          menuClassName="lux-track-select-menu"
          disabled={streams.length <= 1}
          onChange={onSelect}
          aria-labelledby={headingId}
        />
      </div>
    </div>
  );
}
