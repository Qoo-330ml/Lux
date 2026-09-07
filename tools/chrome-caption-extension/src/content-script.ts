const PAGE_SOURCE = "lux-caption-page";
const EXTENSION_SOURCE = "lux-caption-extension";
const PROTOCOL_VERSION = 1;

window.addEventListener("message", (event: MessageEvent<Record<string, unknown>>) => {
  if (event.source !== window || event.origin !== window.location.origin) return;
  const message = event.data;
  if (!message || message.source !== PAGE_SOURCE || message.version !== PROTOCOL_VERSION) return;
  chrome.runtime.sendMessage(message, (response) => {
    if (chrome.runtime.lastError || !response || typeof response !== "object") return;
    postResponse(response as Record<string, unknown>);
  });
});

chrome.runtime.onMessage.addListener((message) => {
  if (!message || typeof message !== "object") return;
  const event = message as Record<string, unknown>;
  if (event.source !== EXTENSION_SOURCE || event.version !== PROTOCOL_VERSION) return;
  postResponse(event);
});

function postResponse(message: Record<string, unknown>) {
  if (typeof message.sessionId !== "string") return;
  window.postMessage(message, window.location.origin);
}
