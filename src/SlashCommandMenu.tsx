import { type KeyboardEvent, type RefObject, useState } from "react";

export type SlashCommand = {
  name: string;
  description?: string | null;
  inputHint?: string | null;
};

type UseSlashCommandOptions = {
  commands: SlashCommand[];
  value: string;
  onChange: (nextValue: string) => void;
  disabled?: boolean;
  inputRef: RefObject<HTMLInputElement | HTMLTextAreaElement | null>;
};

export function useSlashCommandAutocomplete({
  commands,
  value,
  onChange,
  disabled = false,
  inputRef,
}: UseSlashCommandOptions) {
  const [dismissed, setDismissed] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);

  const cursor = inputRef.current?.selectionStart ?? value.length;
  const textBeforeCursor = value.slice(0, cursor);
  const match = textBeforeCursor.match(/(?:^|\n)(\/([^\s]*))$/);
  const slashQuery = match ? match[2].toLowerCase() : null;
  const slashTokenLength = match ? match[1].length : 0;

  const filteredCommands = slashQuery === null
    ? []
    : [
      ...commands.filter((command) => command.name.toLowerCase().startsWith(slashQuery)),
      ...commands.filter(
        (command) => !command.name.toLowerCase().startsWith(slashQuery) && command.name.toLowerCase().includes(slashQuery),
      ),
    ];

  const isOpen = !disabled && !dismissed && slashQuery !== null && filteredCommands.length > 0;
  const clampedIndex = filteredCommands.length > 0 ? Math.min(activeIndex, filteredCommands.length - 1) : 0;

  function selectCommand(command: SlashCommand) {
    const tokenStart = cursor - slashTokenLength;
    const inserted = `/${command.name} `;
    const nextValue = `${value.slice(0, tokenStart)}${inserted}${value.slice(cursor)}`;
    onChange(nextValue);
    setDismissed(true);
    setActiveIndex(0);
    const nextCursor = tokenStart + inserted.length;
    requestAnimationFrame(() => {
      const element = inputRef.current;
      if (!element) return;
      element.focus();
      if (typeof element.setSelectionRange === "function") {
        element.setSelectionRange(nextCursor, nextCursor);
      }
    });
  }

  function handleChange(nextValue: string) {
    onChange(nextValue);
    setDismissed(false);
    setActiveIndex(0);
  }

  function handleKeyDown(event: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) {
    if (!isOpen) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActiveIndex((current) => (current + 1) % filteredCommands.length);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIndex((current) => (current - 1 + filteredCommands.length) % filteredCommands.length);
    } else if (event.key === "Enter" || event.key === "Tab") {
      const chosen = filteredCommands[clampedIndex];
      if (chosen) {
        event.preventDefault();
        selectCommand(chosen);
      }
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      setDismissed(true);
    }
  }

  return {
    isOpen,
    activeIndex: clampedIndex,
    filteredCommands,
    setActiveIndex,
    selectCommand,
    handleChange,
    handleKeyDown,
  };
}

type SlashCommandMenuProps = {
  isOpen: boolean;
  commands: SlashCommand[];
  activeIndex: number;
  onSelect: (command: SlashCommand) => void;
  onHover: (index: number) => void;
};

export function SlashCommandMenu({
  isOpen,
  commands,
  activeIndex,
  onSelect,
  onHover,
}: SlashCommandMenuProps) {
  if (!isOpen) return null;

  return (
    <div className="slash-command-menu" role="listbox" aria-label="Slash commands">
      {commands.map((command, index) => (
        <button
          key={command.name}
          type="button"
          role="option"
          aria-selected={index === activeIndex}
          className={`slash-command-item ${index === activeIndex ? "active" : ""}`}
          onMouseDown={(event) => {
            event.preventDefault();
            onSelect(command);
          }}
          onMouseEnter={() => onHover(index)}
        >
          <div className="slash-command-main">
            <span className="slash-command-name">/{command.name}</span>
            {command.inputHint && <span className="slash-command-hint">{command.inputHint}</span>}
          </div>
          {command.description && <span className="slash-command-desc">{command.description}</span>}
        </button>
      ))}
    </div>
  );
}
