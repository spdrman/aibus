import { useState } from 'react';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { Button, Checkbox, Dialog, DialogActions, FormRow, ReadOnlyRow, Tabs, TextBox } from './index.js';

describe('Button', () => {
  it('defaults to type=button and does not submit a form', async () => {
    const onSubmit = vi.fn((e) => e.preventDefault());
    render(
      <form onSubmit={onSubmit}>
        <Button>Save</Button>
      </form>,
    );
    expect(screen.getByRole('button', { name: 'Save' })).toHaveAttribute('type', 'button');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('is disabled and busy while loading', () => {
    render(<Button loading>Save</Button>);
    const b = screen.getByRole('button', { name: 'Save' });
    expect(b).toBeDisabled();
    expect(b).toHaveAttribute('aria-busy', 'true');
  });
});

describe('form controls', () => {
  it('TextBox labels the input and ties error text to it', async () => {
    const onChange = vi.fn();
    render(<TextBox label="Name" error="Required" onChange={onChange} />);
    const input = screen.getByLabelText('Name');
    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(input).toHaveAccessibleDescription('Required');
    await userEvent.type(input, 'ab');
    expect(onChange).toHaveBeenCalledTimes(2);
  });

  it('TextBox helper text describes the field when there is no error', () => {
    render(<TextBox label="Name" helperText="As on the card" />);
    expect(screen.getByLabelText('Name')).toHaveAccessibleDescription('As on the card');
    expect(screen.getByLabelText('Name')).not.toHaveAttribute('aria-invalid');
  });

  it('Checkbox toggles from the label and from Space', async () => {
    render(<Checkbox label="Remember me" />);
    const box = screen.getByRole('checkbox', { name: 'Remember me' });
    await userEvent.click(screen.getByText('Remember me'));
    expect(box).toBeChecked();
    box.focus();
    await userEvent.keyboard(' ');
    expect(box).not.toBeChecked();
  });

  it('Checkbox sets the indeterminate property', () => {
    render(<Checkbox label="Some" indeterminate onChange={() => {}} />);
    expect((screen.getByRole('checkbox') as HTMLInputElement).indeterminate).toBe(true);
  });

  it('FormRow associates its label with the field it renders', () => {
    render(<FormRow label="Email">{(id) => <input id={id} />}</FormRow>);
    expect(screen.getByLabelText('Email')).toBeInTheDocument();
  });

  it('ReadOnlyRow says "Not set" for an empty value', () => {
    render(
      <dl>
        <ReadOnlyRow label="Phone" value="  " />
        <ReadOnlyRow label="City" value="Oslo" />
      </dl>,
    );
    expect(screen.getByText('Not set')).toBeInTheDocument();
    expect(screen.getByText('Oslo')).toBeInTheDocument();
  });
});

function Harness({ nested = false, disableEscapeClose = false }: { nested?: boolean; disableEscapeClose?: boolean }) {
  const [open, setOpen] = useState(false);
  const [inner, setInner] = useState(false);
  return (
    <>
      <Button onClick={() => setOpen(true)}>Open</Button>
      <Dialog
        open={open}
        onClose={() => setOpen(false)}
        title="Outer"
        disableEscapeClose={disableEscapeClose}
        footer={
          <DialogActions>
            <Button variant="quiet" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button>OK</Button>
          </DialogActions>
        }
      >
        <TextBox label="Name" data-autofocus />
        {nested ? <Button onClick={() => setInner(true)}>Open inner</Button> : null}
        <Dialog open={inner} onClose={() => setInner(false)} title="Inner">
          <Button>Inner action</Button>
        </Dialog>
      </Dialog>
    </>
  );
}

describe('Dialog', () => {
  it('has the dialog role, is named by its title and is modal', async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole('button', { name: 'Open' }));
    const d = screen.getByRole('dialog', { name: 'Outer' });
    expect(d).toHaveAttribute('aria-modal', 'true');
  });

  it('moves focus to the data-autofocus field, and back to the opener on Escape', async () => {
    render(<Harness />);
    const opener = screen.getByRole('button', { name: 'Open' });
    await userEvent.click(opener);
    await vi.waitFor(() => expect(screen.getByLabelText('Name')).toHaveFocus());
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(opener).toHaveFocus();
  });

  it('keeps Tab and Shift+Tab inside the panel', async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole('button', { name: 'Open' }));
    await vi.waitFor(() => expect(screen.getByLabelText('Name')).toHaveFocus());
    // Tab stops: close, Name, Cancel, OK.
    await userEvent.tab({ shift: true });
    expect(screen.getByRole('button', { name: 'Close' })).toHaveFocus();
    await userEvent.tab({ shift: true });
    expect(screen.getByRole('button', { name: 'OK' })).toHaveFocus();
    await userEvent.tab();
    expect(screen.getByRole('button', { name: 'Close' })).toHaveFocus();
  });

  it('closes on backdrop click unless disabled, and not on a click inside the panel', async () => {
    render(<Harness />);
    await userEvent.click(screen.getByRole('button', { name: 'Open' }));
    await userEvent.click(screen.getByRole('dialog'));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('dialog').parentElement as HTMLElement);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('Escape closes only the innermost of nested dialogs', async () => {
    render(<Harness nested />);
    await userEvent.click(screen.getByRole('button', { name: 'Open' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open inner' }));
    expect(screen.getAllByRole('dialog')).toHaveLength(2);
    await userEvent.keyboard('{Escape}');
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    expect(screen.getByRole('dialog', { name: 'Outer' })).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('a dialog that disables Escape still blocks the ones beneath it', async () => {
    function Stack() {
      const [inner, setInner] = useState(true);
      return (
        <Dialog open onClose={() => {}} title="Outer">
          <Dialog open={inner} onClose={() => setInner(false)} title="Sticky" disableEscapeClose>
            <button>x</button>
          </Dialog>
        </Dialog>
      );
    }
    render(<Stack />);
    await userEvent.keyboard('{Escape}');
    expect(screen.getAllByRole('dialog')).toHaveLength(2);
  });
});

describe('Tabs', () => {
  const items = [
    { id: 'a', label: 'Trace', content: 'trace body' },
    { id: 'b', label: 'Graph', content: 'graph body' },
    { id: 'c', label: 'Off', content: 'off body', disabled: true },
    { id: 'd', label: 'Logs', content: 'logs body' },
  ];

  it('selects the first enabled tab and shows only its panel', () => {
    render(<Tabs label="Views" items={items} />);
    expect(screen.getByRole('tab', { name: 'Trace' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tabpanel')).toHaveTextContent('trace body');
    expect(screen.getByRole('tabpanel')).toHaveAccessibleName('Trace');
  });

  it('arrows move and wrap over enabled tabs, Home and End jump', async () => {
    render(<Tabs label="Views" items={items} />);
    screen.getByRole('tab', { name: 'Trace' }).focus();
    await userEvent.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Graph' })).toHaveFocus();
    await userEvent.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Logs' })).toHaveFocus();
    expect(screen.getByRole('tabpanel')).toHaveTextContent('logs body');
    await userEvent.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Trace' })).toHaveFocus();
    await userEvent.keyboard('{ArrowLeft}');
    expect(screen.getByRole('tab', { name: 'Logs' })).toHaveFocus();
    await userEvent.keyboard('{Home}');
    expect(screen.getByRole('tab', { name: 'Trace' })).toHaveFocus();
    await userEvent.keyboard('{End}');
    expect(screen.getByRole('tab', { name: 'Logs' })).toHaveFocus();
  });

  it('only the selected tab is in the Tab order', () => {
    render(<Tabs label="Views" items={items} />);
    expect(screen.getByRole('tab', { name: 'Trace' })).toHaveAttribute('tabindex', '0');
    expect(screen.getByRole('tab', { name: 'Graph' })).toHaveAttribute('tabindex', '-1');
  });

  it('supports controlled selection', async () => {
    const onSelect = vi.fn();
    render(<Tabs label="Views" items={items} selectedId="b" onSelect={onSelect} />);
    expect(screen.getByRole('tabpanel')).toHaveTextContent('graph body');
    await userEvent.click(screen.getByRole('tab', { name: 'Logs' }));
    expect(onSelect).toHaveBeenCalledWith('d');
    expect(screen.getByRole('tabpanel')).toHaveTextContent('graph body');
  });
});
