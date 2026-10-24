// Adapted from the pinned GPUI examples/input.rs (Apache-2.0).
// `multiline` adds hard and soft line breaks, vertical movement, and vertical scrolling.
use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollWheelEvent, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle,
    Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        Newline,
        LineUp,
        LineDown,
        SelectLineUp,
        SelectLineDown,
    ]
);

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    scroll_x: Pixels,
    undo: Vec<(SharedString, Range<usize>)>,
    redo: Vec<(SharedString, Range<usize>)>,
    last_insertion_end: Option<usize>,
    label: SharedString,
    /// Visible rows; 1 is a single-line field.
    rows: usize,
    last_lines: Vec<WrappedLine>,
    last_line_height: Pixels,
    scroll_y: Pixels,
    max_scroll_y: Pixels,
    /// Cursor offset and length when the cursor was last scrolled into view.
    revealed: Option<(usize, usize)>,
}

/// Position of a byte offset within shaped hard lines, relative to the text origin.
fn position_in(lines: &[WrappedLine], offset: usize, line_height: Pixels) -> Point<Pixels> {
    let mut start = 0;
    let mut y = px(0.);
    for line in lines {
        let end = start + line.len();
        if offset <= end {
            let position = line
                .position_for_index(offset - start, line_height)
                .unwrap_or_default();
            return point(position.x, y + position.y);
        }
        y += line.size(line_height).height;
        start = end + 1;
    }
    point(px(0.), y)
}

/// The closest byte offset to a point relative to the text origin.
fn offset_in(lines: &[WrappedLine], position: Point<Pixels>, line_height: Pixels) -> usize {
    let mut start = 0;
    let mut y = px(0.);
    for (index, line) in lines.iter().enumerate() {
        let height = line.size(line_height).height;
        if position.y < y + height || index + 1 == lines.len() {
            let local = point(
                position.x.max(px(0.)),
                (position.y - y).max(px(0.)).min(height - px(1.)),
            );
            let (Ok(offset) | Err(offset)) = line.closest_index_for_position(local, line_height);
            return start + offset.min(line.len());
        }
        y += height;
        start += line.len() + 1;
    }
    0
}

impl TextInput {
    fn checkpoint(&mut self, range: &Range<usize>, inserted: &str) {
        let contiguous_insertion = !inserted.is_empty()
            && range.is_empty()
            && self.last_insertion_end == Some(range.start);
        if !contiguous_insertion {
            self.undo
                .push((self.content.clone(), self.selected_range.clone()));
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
        }
        self.last_insertion_end =
            (!inserted.is_empty() && range.is_empty()).then_some(range.start + inserted.len());
        self.redo.clear();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((content, selection)) = self.undo.pop() {
            self.redo
                .push((self.content.clone(), self.selected_range.clone()));
            self.content = content;
            self.selected_range = selection;
            self.marked_range = None;
            self.selection_reversed = false;
            self.last_insertion_end = None;
            cx.notify();
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((content, selection)) = self.redo.pop() {
            self.undo
                .push((self.content.clone(), self.selected_range.clone()));
            self.content = content;
            self.selected_range = selection;
            self.marked_range = None;
            self.selection_reversed = false;
            self.last_insertion_end = None;
            cx.notify();
        }
    }

    fn word_boundary(&self, forward: bool) -> usize {
        let cursor = self.cursor_offset();
        if forward {
            self.content
                .unicode_word_indices()
                .map(|(index, word)| index + word.len())
                .find(|end| *end > cursor)
                .unwrap_or(self.content.len())
        } else {
            self.content
                .unicode_word_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index < cursor)
                .last()
                .unwrap_or(0)
        }
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.word_boundary(false), cx);
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.word_boundary(true), cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.word_boundary(false), cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.word_boundary(true), cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let start = if self.is_multiline() {
            self.content[..cursor]
                .rfind('\n')
                .map_or(0, |index| index + 1)
        } else {
            0
        };
        self.move_to(start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let end = if self.is_multiline() {
            self.content[cursor..]
                .find('\n')
                .map_or(self.content.len(), |index| cursor + index)
        } else {
            self.content.len()
        };
        self.move_to(end, cx);
    }

    fn is_multiline(&self) -> bool {
        self.rows > 1
    }

    /// The offset one visual row above or below the cursor.
    fn vertical(&self, down: bool) -> usize {
        let height = self.last_line_height;
        let position = position_in(&self.last_lines, self.cursor_offset(), height);
        let y = if down {
            position.y + height * 1.5
        } else {
            position.y - height * 0.5
        };
        let total: Pixels = self
            .last_lines
            .iter()
            .map(|line| line.size(height).height)
            .sum();
        if y < px(0.) {
            0
        } else if y >= total {
            self.content.len()
        } else {
            offset_in(&self.last_lines, point(position.x, y), height)
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }
    fn line_up(&mut self, _: &LineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical(false), cx);
    }
    fn line_down(&mut self, _: &LineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.vertical(true), cx);
    }
    fn select_line_up(&mut self, _: &SelectLineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical(false), cx);
    }
    fn select_line_down(&mut self, _: &SelectLineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical(true), cx);
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(window.line_height());
        self.scroll_y = (self.scroll_y - delta.y).clamp(px(0.), self.max_scroll_y);
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                window.play_system_bell();
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                window.play_system_bell();
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.is_selecting = true;

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.last_insertion_end = None;
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.last_insertion_end = None;
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.last_insertion_end = None;
        self.selection_reversed = false;
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if self.is_multiline() {
            let Some(bounds) = self.last_bounds else {
                return 0;
            };
            let local = position - bounds.origin + point(px(0.), self.scroll_y);
            return offset_in(&self.last_lines, local, self.last_line_height)
                .min(self.content.len());
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left() + self.scroll_x)
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.last_insertion_end = None;
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: "Search titles, label:ready-for-agent, repo:owner/name…".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            scroll_x: px(0.),
            undo: Vec::new(),
            redo: Vec::new(),
            last_insertion_end: None,
            label: "Search loaded open issues".into(),
            rows: 1,
            last_lines: Vec::new(),
            last_line_height: px(24.),
            scroll_y: px(0.),
            max_scroll_y: px(0.),
            revealed: None,
        }
    }

    /// Accept line breaks and show `rows` lines, scrolling vertically beyond them.
    pub fn multiline(mut self, rows: usize) -> Self {
        self.rows = rows.max(2);
        self
    }

    pub fn with_label(mut self, label: &str) -> Self {
        self.label = label.to_owned().into();
        self
    }

    /// Line breaks are kept in multiline fields and become spaces otherwise.
    fn clean(&self, text: &str) -> String {
        if self.is_multiline() {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n'], " ")
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.to_owned().into();
        self
    }

    /// Replace the whole text, clearing undo history and moving the cursor to the end.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.to_owned().into();
        self.selected_range = self.content.len()..self.content.len();
        self.marked_range = None;
        self.undo.clear();
        self.redo.clear();
        self.last_insertion_end = None;
        self.scroll_x = px(0.);
        self.scroll_y = px(0.);
        self.revealed = None;
        cx.notify();
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let new_text = self.clean(new_text);
        let new_text = new_text.as_str();
        if self.marked_range.is_none() {
            self.checkpoint(&range, new_text);
        }
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let new_text = self.clean(new_text);
        let new_text = new_text.as_str();
        if self.marked_range.is_none() {
            self.checkpoint(&range, new_text);
        }
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|selection| {
                let offset = |n| {
                    new_text
                        .chars()
                        .scan(0, |count, ch| {
                            let before = *count;
                            *count += ch.len_utf16();
                            Some((before, ch.len_utf8()))
                        })
                        .take_while(|(before, _)| *before < n)
                        .map(|(_, bytes)| bytes)
                        .sum::<usize>()
                };
                range.start + offset(selection.start)..range.start + offset(selection.end)
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if self.is_multiline() {
            let height = self.last_line_height;
            let origin = bounds.origin - point(px(0.), self.scroll_y);
            let start = position_in(&self.last_lines, range.start, height);
            let end = position_in(&self.last_lines, range.end, height);
            return Some(Bounds::from_corners(
                origin + start,
                origin + point(end.x.max(start.x), start.y + height),
            ));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() - self.scroll_x + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() - self.scroll_x + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        if self.is_multiline() {
            let local = point - bounds.origin + gpui::point(px(0.), self.scroll_y);
            let offset = offset_in(&self.last_lines, local, self.last_line_height);
            return Some(self.offset_to_utf16(offset.min(self.content.len())));
        }
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout
            .closest_index_for_x(point.x - bounds.left() + self.scroll_x)
            .min(self.content.len());
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), rgb(super::MUTED).into())
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let available = (bounds.size.width - px(3.)).max(px(0.));
        let scroll_x = input
            .scroll_x
            .min(cursor_pos)
            .max(cursor_pos - available)
            .max(px(0.));
        let origin_x = bounds.left() - scroll_x;
        self.input.update(cx, |input, _| input.scroll_x = scroll_x);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(origin_x + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    rgb(super::ACCENT),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            origin_x + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            origin_x + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    rgba(0x9ec5f850),
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(
            point(bounds.left() - self.input.read(cx).scroll_x, bounds.top()),
            window.line_height(),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        )
        .unwrap();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

/// The multiline counterpart of `TextElement`: wrapped lines in a fixed number of rows.
struct AreaElement {
    input: Entity<TextInput>,
}

struct AreaState {
    lines: Vec<WrappedLine>,
    placeholder: Option<ShapedLine>,
    quads: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for AreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AreaElement {
    type RequestLayoutState = ();
    type PrepaintState = AreaState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let rows = self.input.read(cx).rows as f32;
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * rows).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let height = window.line_height();
        let run = TextRun {
            len: content.len(),
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs: Vec<TextRun> = match input.marked_range.as_ref() {
            Some(marked) => vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: content.len() - marked.end,
                    ..run.clone()
                },
            ],
            None => vec![run.clone()],
        };
        let placeholder = content.is_empty().then(|| {
            let text = input.placeholder.clone();
            let runs = [TextRun {
                len: text.len(),
                color: rgb(super::MUTED).into(),
                ..run.clone()
            }];
            window
                .text_system()
                .shape_line(text, font_size, &runs, None)
        });
        let wrap = (bounds.size.width - px(3.)).max(px(1.));
        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(content.clone(), font_size, &runs, Some(wrap), None)
            .map(|lines| lines.into_iter().collect())
            .unwrap_or_default();

        let total: Pixels = lines.iter().map(|line| line.size(height).height).sum();
        let max_scroll = (total - bounds.size.height).max(px(0.));
        let caret = position_in(&lines, cursor, height);
        let mut scroll = input.scroll_y;
        if input.revealed != Some((cursor, content.len())) {
            scroll = scroll
                .min(caret.y)
                .max(caret.y + height - bounds.size.height);
        }
        let scroll = scroll.clamp(px(0.), max_scroll);
        self.input.update(cx, |input, _| {
            input.scroll_y = scroll;
            input.max_scroll_y = max_scroll;
            input.revealed = Some((cursor, content.len()));
            input.last_line_height = height;
        });
        let origin = bounds.origin - point(px(0.), scroll);
        let right = bounds.size.width;
        let mut quads = vec![];
        let highlight = |left: Pixels, right: Pixels, top: Pixels| {
            fill(
                Bounds::from_corners(
                    origin + point(left, top),
                    origin + point(right, top + height),
                ),
                rgba(0x9ec5f850),
            )
        };
        if !selected_range.is_empty() {
            let start = position_in(&lines, selected_range.start, height);
            let end = position_in(&lines, selected_range.end, height);
            if start.y == end.y {
                quads.push(highlight(start.x, end.x, start.y));
            } else {
                quads.push(highlight(start.x, right, start.y));
                let mut row = start.y + height;
                while row < end.y {
                    quads.push(highlight(px(0.), right, row));
                    row += height;
                }
                quads.push(highlight(px(0.), end.x, end.y));
            }
        }
        let cursor = selected_range.is_empty().then(|| {
            fill(
                Bounds::new(origin + caret, size(px(2.), height)),
                rgb(super::ACCENT),
            )
        });
        AreaState {
            lines,
            placeholder,
            quads,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let height = window.line_height();
        let scroll = self.input.read(cx).scroll_y;
        let lines = std::mem::take(&mut prepaint.lines);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in prepaint.quads.drain(..) {
                window.paint_quad(quad);
            }
            if let Some(placeholder) = prepaint.placeholder.take() {
                let _ = placeholder.paint(
                    bounds.origin,
                    height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            let mut y = bounds.top() - scroll;
            for line in &lines {
                let _ = line.paint(
                    point(bounds.left(), y),
                    height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                y += line.size(height).height;
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        self.input.update(cx, |input, _cx| {
            input.last_lines = lines;
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let field = div()
            .id("text-input")
            .aria_label(self.label.clone())
            .aria_value(self.content.clone())
            .flex()
            .w_full()
            // Clipping removes the automatic minimum height, so a scrolling column would
            // otherwise shrink the field.
            .flex_none()
            .overflow_hidden()
            .key_context("TextInput")
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .bg(rgb(super::BG))
            .border_1()
            .border_color(rgb(if focused { super::ACCENT } else { super::LINE }))
            .rounded_md()
            .line_height(px(24.))
            .text_size(px(14.))
            .font_family(".AppleSystemUIFont")
            .px_3()
            .py_2();
        if !self.is_multiline() {
            return field
                .role(gpui::Role::TextInput)
                .tab_stop(true)
                .track_focus(&self.focus_handle(cx))
                .child(TextElement { input: cx.entity() });
        }
        // The inner context lets Enter and the arrow keys edit lines instead of leaving the field.
        field
            .role(gpui::Role::MultilineTextInput)
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(
                div()
                    .w_full()
                    .key_context("MultilineInput")
                    .tab_stop(true)
                    .track_focus(&self.focus_handle(cx))
                    .on_action(cx.listener(Self::newline))
                    .on_action(cx.listener(Self::line_up))
                    .on_action(cx.listener(Self::line_down))
                    .on_action(cx.listener(Self::select_line_up))
                    .on_action(cx.listener(Self::select_line_down))
                    .child(AreaElement { input: cx.entity() }),
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("secondary-z", Undo, Some("TextInput")),
        KeyBinding::new("secondary-shift-z", Redo, Some("TextInput")),
        KeyBinding::new("secondary-a", SelectAll, Some("TextInput")),
        KeyBinding::new("secondary-v", Paste, Some("TextInput")),
        KeyBinding::new("secondary-c", Copy, Some("TextInput")),
        KeyBinding::new("secondary-x", Cut, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("j", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("k", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("/", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("enter", Newline, Some("MultilineInput")),
        KeyBinding::new("shift-enter", Newline, Some("MultilineInput")),
        KeyBinding::new("up", LineUp, Some("MultilineInput")),
        KeyBinding::new("down", LineDown, Some("MultilineInput")),
        KeyBinding::new("shift-up", SelectLineUp, Some("MultilineInput")),
        KeyBinding::new("shift-down", SelectLineDown, Some("MultilineInput")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("alt-left", WordLeft, Some("TextInput")),
        KeyBinding::new("alt-right", WordRight, Some("TextInput")),
        KeyBinding::new("alt-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("alt-shift-right", SelectWordRight, Some("TextInput")),
        KeyBinding::new("cmd-left", Home, Some("TextInput")),
        KeyBinding::new("cmd-right", End, Some("TextInput")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("TextInput")),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new("ctrl-left", WordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-right", WordRight, Some("TextInput")),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some("TextInput")),
    ]);
}
