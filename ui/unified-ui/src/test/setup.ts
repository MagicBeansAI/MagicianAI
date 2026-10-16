import '@testing-library/jest-dom/vitest';

import { installBrowserTestPolyfills, installTestStorage } from './browser';

installTestStorage();
if (typeof window !== 'undefined') installBrowserTestPolyfills();
