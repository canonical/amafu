import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router";

import Application from "./application";
import { SessionProvider } from "./session";
import "./style.scss";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <BrowserRouter>
        <SessionProvider>
          <Application />
        </SessionProvider>
      </BrowserRouter>
    </StrictMode>,
  );
}
