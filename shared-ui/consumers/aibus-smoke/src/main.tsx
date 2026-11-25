import '@sharedui/design-tokens/tokens.css';
import '@sharedui/react-primitives/styles.css';
import { createRoot } from 'react-dom/client';
import { Showcase } from './Showcase.js';

const params = new URLSearchParams(location.search);
const theme = params.get('theme');
if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
document.body.style.cssText = 'margin:0;background:var(--ui-bg-app);color:var(--ui-text-primary);font-family:var(--ui-font-sans)';

createRoot(document.getElementById('root')!).render(<Showcase />);
