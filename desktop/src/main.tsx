import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

try {
  if (localStorage.getItem("theme") === "light") document.documentElement.classList.remove("dark");
} catch { /* storage unavailable */ }

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
