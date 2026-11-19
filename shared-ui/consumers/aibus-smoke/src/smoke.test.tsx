import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { colorTokens } from '@sharedui/design-tokens';
import { Showcase } from './Showcase.js';

// Imports the packages by name, so this runs against their built output (dist), not their source.
describe('aibus consumer smoke', () => {
  it('renders the controls, a dialog and the tabs from the shared packages', async () => {
    render(<Showcase />);
    expect(screen.getByRole('tablist', { name: 'Showcase sections' })).toBeInTheDocument();
    expect(screen.getByLabelText('Workflow name')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('tab', { name: 'Dialog' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open dialog' }));
    expect(screen.getByRole('dialog', { name: 'Delete workflow' })).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('gets the token data from the package', () => {
    expect(colorTokens.dark['bg-app']).toBe('#0B1120');
  });
});
