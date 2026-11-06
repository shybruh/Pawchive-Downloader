// shadcn/ui-style primitives (same class recipes as shadcn's New York style), built on Radix.
import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { Dialog as DialogPrimitive, Select as SelectPrimitive, Slot, Switch as SwitchPrimitive, Tooltip as TooltipPrimitive, DropdownMenu as DropdownPrimitive } from "radix-ui";
import { CheckIcon, ChevronDownIcon, XIcon } from "lucide-react";
import { cn } from "@/lib/utils";

// ── Button ──
export const buttonVariants = cva(
  "inline-flex shrink-0 items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-[color,background-color,border-color,box-shadow] outline-none cursor-default disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 [&_svg]:shrink-0 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground shadow-xs hover:bg-primary/90",
        destructive: "bg-destructive/80 text-white shadow-xs hover:bg-destructive/70",
        outline: "border border-input bg-input/20 shadow-xs hover:bg-accent hover:text-accent-foreground",
        secondary: "bg-secondary text-secondary-foreground shadow-xs hover:bg-secondary/80",
        ghost: "hover:bg-accent hover:text-accent-foreground",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-9 px-4 py-2 has-[>svg]:px-3",
        sm: "h-8 gap-1.5 rounded-md px-3 has-[>svg]:px-2.5",
        xs: "h-7 gap-1 rounded-md px-2 text-xs [&_svg:not([class*='size-'])]:size-3.5",
        lg: "h-10 rounded-md px-6 has-[>svg]:px-4",
        icon: "size-9",
        "icon-sm": "size-8",
        "icon-xs": "size-7 [&_svg:not([class*='size-'])]:size-3.5",
      },
    },
    defaultVariants: { variant: "default", size: "default" },
  },
);

export function Button({ className, variant, size, asChild = false, ...props }:
  React.ComponentProps<"button"> & VariantProps<typeof buttonVariants> & { asChild?: boolean }) {
  const Comp = asChild ? Slot.Root : "button";
  return <Comp className={cn(buttonVariants({ variant, size, className }))} {...props} />;
}

// ── Input / Textarea / Label ──
export function Input({ className, ...props }: React.ComponentProps<"input">) {
  return (
    <input
      className={cn(
        "h-9 w-full min-w-0 rounded-md border border-input bg-input/20 px-3 py-1 text-sm shadow-xs outline-none transition-[color,box-shadow] placeholder:text-muted-foreground disabled:opacity-50",
        "focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
        className,
      )}
      spellCheck={false}
      {...props}
    />
  );
}

export function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return (
    <textarea
      className={cn(
        "min-h-16 w-full rounded-md border border-input bg-input/20 px-3 py-2 text-sm shadow-xs outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
        className,
      )}
      spellCheck={false}
      {...props}
    />
  );
}

export function Label({ className, ...props }: React.ComponentProps<"label">) {
  return <label className={cn("text-sm font-medium leading-none select-none", className)} {...props} />;
}

// ── Card ──
export function Card({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("flex flex-col gap-5 rounded-xl border bg-card py-5 text-card-foreground shadow-sm", className)} {...props} />;
}
export function CardHeader({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("grid auto-rows-min grid-rows-[auto_auto] items-start gap-1 px-5 has-[[data-slot=card-action]]:grid-cols-[1fr_auto]", className)} {...props} />;
}
export function CardTitle({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("leading-none font-semibold", className)} {...props} />;
}
export function CardDescription({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("text-sm text-muted-foreground", className)} {...props} />;
}
export function CardAction({ className, ...props }: React.ComponentProps<"div">) {
  return <div data-slot="card-action" className={cn("col-start-2 row-span-2 row-start-1 self-start justify-self-end", className)} {...props} />;
}
export function CardContent({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("px-5", className)} {...props} />;
}

// ── Badge ──
const badgeVariants = cva(
  "inline-flex w-fit shrink-0 items-center justify-center gap-1 whitespace-nowrap rounded-md border px-2 py-0.5 text-xs font-medium [&>svg]:size-3",
  {
    variants: {
      variant: {
        default: "border-transparent bg-primary text-primary-foreground",
        secondary: "border-transparent bg-secondary text-secondary-foreground",
        outline: "text-foreground",
        success: "border-success/25 bg-success/10 text-success",
        warning: "border-warning/25 bg-warning/10 text-warning",
        destructive: "border-destructive/25 bg-destructive/10 text-destructive",
        info: "border-info/25 bg-info/10 text-info",
      },
    },
    defaultVariants: { variant: "secondary" },
  },
);
export function Badge({ className, variant, ...props }: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />;
}

// ── Separator / Progress / Kbd / Skeleton ──
export function Separator({ className, orientation = "horizontal" }: { className?: string; orientation?: "horizontal" | "vertical" }) {
  return <div className={cn("shrink-0 bg-border", orientation === "horizontal" ? "h-px w-full" : "h-full w-px", className)} />;
}

export function Progress({ value = 0, className, indicatorClassName }: { value?: number; className?: string; indicatorClassName?: string }) {
  return (
    <div className={cn("relative h-1.5 w-full overflow-hidden rounded-full bg-primary/15", className)}>
      <div className={cn("h-full bg-primary transition-[width] duration-300", indicatorClassName)} style={{ width: `${Math.max(0, Math.min(100, value))}%` }} />
    </div>
  );
}

export function Kbd({ className, ...props }: React.ComponentProps<"kbd">) {
  return <kbd className={cn("pointer-events-none inline-flex h-5 items-center rounded border bg-muted px-1.5 font-mono text-[10px] font-medium text-muted-foreground", className)} {...props} />;
}

// ── Switch ──
export function Switch({ className, ...props }: React.ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        "peer inline-flex h-[1.15rem] w-8 shrink-0 items-center rounded-full border border-transparent shadow-xs outline-none transition-all focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:opacity-50 data-[state=checked]:bg-primary data-[state=unchecked]:bg-input",
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb className="pointer-events-none block size-4 rounded-full bg-background ring-0 transition-transform data-[state=checked]:translate-x-[calc(100%-2px)] data-[state=unchecked]:translate-x-0 dark:data-[state=checked]:bg-primary-foreground dark:data-[state=unchecked]:bg-foreground" />
    </SwitchPrimitive.Root>
  );
}

// ── Select ──
export function Select({ value, onValueChange, options, className, placeholder, size = "default" }: {
  value: string; onValueChange: (v: string) => void; options: { value: string; label: React.ReactNode }[];
  className?: string; placeholder?: string; size?: "sm" | "default";
}) {
  return (
    <SelectPrimitive.Root value={value} onValueChange={onValueChange}>
      <SelectPrimitive.Trigger
        className={cn(
          "flex w-fit items-center justify-between gap-2 whitespace-nowrap rounded-md border border-input bg-input/20 px-3 text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 data-[placeholder]:text-muted-foreground [&_svg]:size-4",
          size === "sm" ? "h-8" : "h-9",
          className,
        )}
      >
        <SelectPrimitive.Value placeholder={placeholder} />
        <SelectPrimitive.Icon asChild><ChevronDownIcon className="opacity-50" /></SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
      <SelectPrimitive.Portal>
        <SelectPrimitive.Content position="popper" sideOffset={4}
          className="relative z-50 max-h-(--radix-select-content-available-height) min-w-[8rem] overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md data-[state=open]:animate-in">
          <SelectPrimitive.Viewport className="w-full min-w-[var(--radix-select-trigger-width)]">
            {options.map((o) => (
              <SelectPrimitive.Item key={o.value} value={o.value}
                className="relative flex w-full items-center gap-2 rounded-sm py-1.5 pr-8 pl-2 text-sm outline-none select-none data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground">
                <span className="absolute right-2 flex size-3.5 items-center justify-center">
                  <SelectPrimitive.ItemIndicator><CheckIcon className="size-4" /></SelectPrimitive.ItemIndicator>
                </span>
                <SelectPrimitive.ItemText>{o.label}</SelectPrimitive.ItemText>
              </SelectPrimitive.Item>
            ))}
          </SelectPrimitive.Viewport>
        </SelectPrimitive.Content>
      </SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  );
}

// ── Tooltip ──
export const TooltipProvider = TooltipPrimitive.Provider;
export function Tip({ content, children, side = "top" }: { content: React.ReactNode; children: React.ReactNode; side?: "top" | "bottom" | "left" | "right" }) {
  if (!content) return <>{children}</>;
  return (
    <TooltipPrimitive.Root delayDuration={350}>
      <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content side={side} sideOffset={6}
          className="z-50 max-w-xs rounded-md bg-primary px-2.5 py-1.5 text-xs text-primary-foreground shadow-md">
          {content}
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}

// ── Dialog ──
export const Dialog = DialogPrimitive.Root;
export function DialogContent({ className, children, title, description }: { className?: string; children: React.ReactNode; title: React.ReactNode; description?: React.ReactNode }) {
  return (
    <DialogPrimitive.Portal>
      <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/60 backdrop-blur-[2px]" />
      <DialogPrimitive.Content
        className={cn("fixed top-1/2 left-1/2 z-50 grid w-full max-w-lg -translate-x-1/2 -translate-y-1/2 gap-4 rounded-xl border bg-background p-6 shadow-lg outline-none", className)}>
        <div className="flex flex-col gap-1.5">
          <DialogPrimitive.Title className="text-lg leading-none font-semibold">{title}</DialogPrimitive.Title>
          {description ? <DialogPrimitive.Description className="text-sm text-muted-foreground">{description}</DialogPrimitive.Description> : <DialogPrimitive.Description className="sr-only">{String(title)}</DialogPrimitive.Description>}
        </div>
        {children}
        <DialogPrimitive.Close className="absolute top-4 right-4 rounded-xs opacity-70 transition-opacity hover:opacity-100 [&_svg]:size-4">
          <XIcon /><span className="sr-only">Close</span>
        </DialogPrimitive.Close>
      </DialogPrimitive.Content>
    </DialogPrimitive.Portal>
  );
}
export function DialogFooter({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("flex flex-col-reverse gap-2 sm:flex-row sm:justify-end", className)} {...props} />;
}

// ── Dropdown menu ──
export const DropdownMenu = DropdownPrimitive.Root;
export const DropdownMenuTrigger = DropdownPrimitive.Trigger;
export function DropdownMenuContent({ className, align = "end", container, ...props }: React.ComponentProps<typeof DropdownPrimitive.Content> & { container?: HTMLElement | null }) {
  return (
    <DropdownPrimitive.Portal container={container ?? undefined}>
      <DropdownPrimitive.Content align={align} sideOffset={4}
        className={cn("z-50 min-w-[10rem] overflow-hidden rounded-md border bg-popover p-1 text-popover-foreground shadow-md", className)} {...props} />
    </DropdownPrimitive.Portal>
  );
}
export function DropdownMenuItem({ className, variant, ...props }: React.ComponentProps<typeof DropdownPrimitive.Item> & { variant?: "destructive" }) {
  return (
    <DropdownPrimitive.Item
      className={cn(
        "relative flex items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-none select-none data-[disabled]:opacity-50 data-[highlighted]:bg-accent [&_svg]:size-4 [&_svg]:text-muted-foreground",
        variant === "destructive" && "text-destructive data-[highlighted]:bg-destructive/10 [&_svg]:!text-destructive",
        className,
      )}
      {...props}
    />
  );
}
export function DropdownMenuSeparator() { return <DropdownPrimitive.Separator className="-mx-1 my-1 h-px bg-border" />; }
export function DropdownMenuLabel({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("px-2 py-1.5 text-xs font-medium text-muted-foreground", className)} {...props} />;
}

// ── Segmented control (shadcn Tabs list look) ──
export function Segmented<T extends string>({ value, onChange, options, className }: {
  value: T; onChange: (v: T) => void; options: { value: T; label: React.ReactNode; count?: number }[]; className?: string;
}) {
  return (
    <div className={cn("inline-flex h-8 w-fit items-center rounded-lg bg-muted p-[3px] text-muted-foreground", className)}>
      {options.map((o) => (
        <button key={o.value} onClick={() => onChange(o.value)}
          className={cn(
            "inline-flex h-full items-center gap-1.5 rounded-md px-2.5 text-xs font-medium whitespace-nowrap transition-colors",
            value === o.value ? "bg-background text-foreground shadow-sm dark:bg-input/40" : "hover:text-foreground",
          )}>
          {o.label}
          {o.count !== undefined && <span className="tabular-nums text-muted-foreground">{o.count}</span>}
        </button>
      ))}
    </div>
  );
}

// ── Empty state ──
export function Empty({ icon: Icon, title, description, children, className }: {
  icon: React.ComponentType<{ className?: string }>; title: string; description?: React.ReactNode; children?: React.ReactNode; className?: string;
}) {
  return (
    <div className={cn("flex flex-1 flex-col items-center justify-center gap-3 p-10 text-center", className)}>
      <div className="flex size-11 items-center justify-center rounded-lg border bg-card shadow-xs"><Icon className="size-5 text-muted-foreground" /></div>
      <div className="space-y-1">
        <div className="font-medium">{title}</div>
        {description && <div className="max-w-sm text-sm text-muted-foreground">{description}</div>}
      </div>
      {children}
    </div>
  );
}

// ── Alert (one look for every in-page notice) ──
const alertTones = {
  info: { box: "border-info/25 bg-info/10", icon: "text-info" },
  warning: { box: "border-warning/25 bg-warning/10", icon: "text-warning" },
  destructive: { box: "border-destructive/30 bg-destructive/10 text-destructive", icon: "text-destructive" },
  success: { box: "border-success/25 bg-success/10", icon: "text-success" },
  neutral: { box: "bg-card", icon: "text-muted-foreground" },
} as const;
export function Alert({ tone = "neutral", icon: Icon, children, action, className, busy }: {
  tone?: keyof typeof alertTones; icon?: React.ComponentType<{ className?: string }>; children: React.ReactNode; action?: React.ReactNode;
  className?: string; busy?: boolean;
}) {
  const t = alertTones[tone];
  return (
    <div className={cn("flex items-center gap-3 rounded-lg border px-3 py-2 text-sm", t.box, className)}>
      {Icon && <Icon className={cn("size-4 shrink-0", t.icon, busy && "animate-spin")} />}
      <div className="min-w-0 flex-1">{children}</div>
      {action}
    </div>
  );
}
