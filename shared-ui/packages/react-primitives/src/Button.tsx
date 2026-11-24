import type { ButtonHTMLAttributes, ReactNode } from 'react';

export type ButtonVariant = 'primary' | 'quiet' | 'danger';
export type ButtonSize = 'sm' | 'md' | 'lg';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Disables the button and sets aria-busy; the label stays so the width does not jump. */
  loading?: boolean;
  iconLeft?: ReactNode;
  iconRight?: ReactNode;
}

/** A native button. `type` defaults to "button" so it never submits a form by accident. */
export function Button({
  variant = 'primary',
  size = 'md',
  loading = false,
  disabled,
  className,
  iconLeft,
  iconRight,
  children,
  type = 'button',
  ...rest
}: ButtonProps) {
  return (
    <button
      {...rest}
      type={type}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      className={['ui-button', `ui-button--${variant}`, `ui-button--${size}`, className].filter(Boolean).join(' ')}
    >
      {iconLeft ? <span aria-hidden="true">{iconLeft}</span> : null}
      {children}
      {iconRight ? <span aria-hidden="true">{iconRight}</span> : null}
    </button>
  );
}
