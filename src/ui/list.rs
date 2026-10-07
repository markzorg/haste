//! Playlist view: `ListStore` → `SortListModel` → `FilterListModel` →
//! `MultiSelection` → `ColumnView`. Only visible rows get widgets, so the
//! list scales to tens of thousands of tracks.

use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

use super::tr;
use crate::playlist::{Column, Track, compare};
use crate::util::fmt_time;

/// The track stored in a list item.
pub fn track(obj: &glib::Object) -> Ref<'_, Track> {
    obj.downcast_ref::<glib::BoxedAnyObject>().expect("track item").borrow::<Track>()
}

pub fn wrap(t: Track) -> glib::Object {
    glib::BoxedAnyObject::new(t).upcast()
}

fn key(obj: &glib::Object) -> usize {
    obj.as_ptr() as usize
}

pub struct List {
    pub store: gio::ListStore,
    pub sorted: gtk::SortListModel,
    pub filtered: gtk::FilterListModel,
    pub selection: gtk::MultiSelection,
    pub view: gtk::ColumnView,
    filter: gtk::CustomFilter,
    query: Rc<RefCell<String>>,
    columns: Vec<(Column, gtk::ColumnViewColumn)>,
    /// Labels currently bound to each item, to restyle the playing row
    /// without emitting model changes.
    bound: Rc<RefCell<HashMap<usize, Vec<gtk::Label>>>>,
    playing: Rc<Cell<usize>>,
}

impl List {
    pub fn new() -> List {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let view = gtk::ColumnView::new(None::<gtk::SelectionModel>);
        view.set_show_column_separators(true);
        view.add_css_class("data-table");
        let bound: Rc<RefCell<HashMap<usize, Vec<gtk::Label>>>> = Rc::default();
        let playing = Rc::new(Cell::new(0usize));
        let mut columns = Vec::new();
        for col in Column::ALL {
            let column = gtk::ColumnViewColumn::new(Some(tr(col_title(col))), Some(factory(col, &bound, &playing)));
            column.set_resizable(true);
            column.set_sorter(Some(&gtk::CustomSorter::new(move |a, b| compare(&track(a), &track(b), col).into())));
            match col {
                Column::Title => column.set_expand(true),
                Column::Duration => column.set_fixed_width(72),
                _ => column.set_fixed_width(200),
            }
            view.append_column(&column);
            columns.push((col, column));
        }
        let sorted = gtk::SortListModel::new(Some(store.clone()), view.sorter());
        let query: Rc<RefCell<String>> = Rc::default();
        let q = query.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            let q = q.borrow();
            q.is_empty() || track(obj).matches(&q)
        });
        let filtered = gtk::FilterListModel::new(Some(sorted.clone()), Some(filter.clone()));
        let selection = gtk::MultiSelection::new(Some(filtered.clone()));
        view.set_model(Some(&selection));
        List { store, sorted, filtered, selection, view, filter, query, columns, bound, playing }
    }

    /// Updates the search query; `q` is matched case-insensitively.
    pub fn set_query(&self, q: &str) {
        let q = q.trim().to_lowercase();
        let old = self.query.replace(q.clone());
        let change = if q == old {
            return;
        } else if q.starts_with(&old) {
            gtk::FilterChange::MoreStrict
        } else if old.starts_with(&q) {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::Different
        };
        self.filter.changed(change);
    }

    pub fn is_filtered(&self) -> bool {
        !self.query.borrow().is_empty()
    }

    /// Marks `obj` as the playing item (bold row).
    pub fn set_playing(&self, obj: Option<&glib::Object>) {
        let new = obj.map_or(0, key);
        let old = self.playing.replace(new);
        let bound = self.bound.borrow();
        for l in bound.get(&old).into_iter().flatten() {
            l.remove_css_class("playing");
        }
        for l in bound.get(&new).into_iter().flatten() {
            l.add_css_class("playing");
        }
    }

    /// Current sort column and whether it is descending.
    pub fn sort_state(&self) -> Option<(Column, bool)> {
        let sorter = self.view.sorter().and_downcast::<gtk::ColumnViewSorter>()?;
        let primary = sorter.primary_sort_column()?;
        let col = self.columns.iter().find(|(_, c)| *c == primary)?.0;
        Some((col, sorter.primary_sort_order() == gtk::SortType::Descending))
    }

    pub fn set_sort(&self, sort: Option<(Column, bool)>) {
        let col = sort.and_then(|(c, _)| self.columns.iter().find(|(x, _)| *x == c)).map(|(_, c)| c);
        let order = if sort.is_some_and(|(_, d)| d) { gtk::SortType::Descending } else { gtk::SortType::Ascending };
        self.view.sort_by_column(col, order);
    }

    /// Position of `obj` in `model` (linear scan, pointer comparison).
    pub fn position_in(model: &impl IsA<gio::ListModel>, obj: &glib::Object) -> Option<u32> {
        let model = model.as_ref();
        (0..model.n_items()).find(|&i| model.item(i).as_ref() == Some(obj))
    }

    /// Selected items in view order.
    pub fn selected(&self) -> Vec<glib::Object> {
        let set = self.selection.selection();
        let mut out = Vec::with_capacity(set.size() as usize);
        if let Some((iter, first)) = gtk::BitsetIter::init_first(&set) {
            out.extend(self.filtered.item(first));
            for pos in iter {
                out.extend(self.filtered.item(pos));
            }
        }
        out
    }
}

fn col_title(col: Column) -> &'static str {
    match col {
        Column::Artist => "Artist",
        Column::Title => "Title",
        Column::Album => "Album",
        Column::Duration => "Time",
    }
}

fn factory(col: Column, bound: &Rc<RefCell<HashMap<usize, Vec<gtk::Label>>>>, playing: &Rc<Cell<usize>>) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, item| {
        let label = gtk::Label::new(None);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        if col == Column::Duration {
            label.set_xalign(1.0);
            label.add_css_class("numeric");
        } else {
            label.set_xalign(0.0);
        }
        item.downcast_ref::<gtk::ListItem>().expect("list item").set_child(Some(&label));
    });
    let (b, p) = (bound.clone(), playing.clone());
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let (Some(obj), Some(label)) = (item.item(), item.child().and_downcast::<gtk::Label>()) else { return };
        let t = track(&obj);
        match col {
            Column::Artist => label.set_text(&t.artist),
            Column::Title => label.set_text(&t.title),
            Column::Album => label.set_text(&t.album),
            Column::Duration => label.set_text(&fmt_time(if t.duration > 0.0 { t.duration } else { -1.0 })),
        }
        if key(&obj) == p.get() {
            label.add_css_class("playing");
        }
        b.borrow_mut().entry(key(&obj)).or_default().push(label);
    });
    let b = bound.clone();
    f.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let (Some(obj), Some(label)) = (item.item(), item.child().and_downcast::<gtk::Label>()) else { return };
        label.remove_css_class("playing");
        let mut bound = b.borrow_mut();
        if let Some(v) = bound.get_mut(&key(&obj)) {
            v.retain(|l| *l != label);
            if v.is_empty() {
                bound.remove(&key(&obj));
            }
        }
    });
    f
}
