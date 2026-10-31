import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { App } from "./app/App";
import { LuxConfirmProvider } from "./components/ui";
import "./react.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("Lux Web root element is missing");
}

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 60_000,
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <LuxConfirmProvider>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </LuxConfirmProvider>
    </QueryClientProvider>
  </StrictMode>,
);
