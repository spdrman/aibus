import "@sharedui/design-tokens/tokens.css";
import "@sharedui/react-primitives/styles.css";
import "dockview-react/dist/styles/dockview.css";
import "./styles.css";
import { createRoot } from "react-dom/client";
import { App } from "./App";

createRoot(document.getElementById("root")!).render(<App />);
