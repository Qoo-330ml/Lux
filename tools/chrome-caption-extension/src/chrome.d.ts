declare const chrome: {
  runtime: {
    onMessage: {
      addListener(listener: (message: unknown, sender: { tab?: { id?: number; url?: string } }, sendResponse: (response: unknown) => void) => boolean | void): void;
    };
    sendMessage(message: unknown, callback?: (response: unknown) => void): void;
    lastError?: { message?: string };
  };
  tabs: {
    sendMessage(tabId: number, message: unknown): Promise<unknown>;
    onRemoved: { addListener(listener: (tabId: number) => void): void };
  };
  declarativeNetRequest: {
    updateDynamicRules(options: { addRules?: unknown[]; removeRuleIds?: number[] }): Promise<void>;
  };
};
