import { AlertCircle, ArrowLeft, RotateCcw } from "lucide-react";

type PlayerLoadingStateProps = {
  message: string;
};

export function PlayerLoadingState({ message }: PlayerLoadingStateProps) {
  return (
    <main className="lux-player-page lux-player-page-loading" aria-busy="true">
      <div className="lux-spinner" aria-hidden="true" />
      <p>{message}</p>
    </main>
  );
}

type PlayerErrorStateProps = {
  title: string;
  message: string;
  onBack: () => void;
  onRetry?: () => void;
};

export function PlayerErrorState({
  title,
  message,
  onBack,
  onRetry,
}: PlayerErrorStateProps) {
  return (
    <main className="lux-player-page lux-player-page-error" role="alert">
      <div className="lux-player-error-card">
        <AlertCircle size={36} className="lux-player-error-icon" aria-hidden="true" />
        <h1>{title}</h1>
        <p>{message}</p>
        <div className="lux-player-error-actions">
          {onRetry ? (
            <button
              className="lux-player-glass-btn"
              type="button"
              onClick={onRetry}
              aria-label="重试"
            >
              <RotateCcw size={16} aria-hidden="true" /> 重试
            </button>
          ) : null}
          <button
            className="lux-player-glass-btn lux-player-glass-btn-primary"
            type="button"
            onClick={onBack}
            aria-label="返回上一页"
          >
            <ArrowLeft size={16} aria-hidden="true" /> 返回上一页
          </button>
        </div>
      </div>
    </main>
  );
}
