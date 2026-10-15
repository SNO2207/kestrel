import { render } from "solid-js/web";

import { QuickLauncher } from "./components/QuickLauncher";
import "./styles.css";
// The tag colours, for the palette's `#` tags.
import "./shortcuts.css";

render(() => <QuickLauncher />, document.getElementById("root")!);
