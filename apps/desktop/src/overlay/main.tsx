import React from "react";
import ReactDOM from "react-dom/client";
import { Overlay } from "./Overlay";
import { parseDisplayParams } from "./marker";

const display = parseDisplayParams(window.location.search);
// Without valid display geometry there is nothing safe to draw: stay empty.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{display && <Overlay display={display} />}</React.StrictMode>,
);
