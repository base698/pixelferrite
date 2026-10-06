/// Integer rectangle, half-open: `x0 <= x < x1`, `y0 <= y < y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub const EMPTY: IRect = IRect { x0: 0, y0: 0, x1: 0, y1: 0 };

    pub fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn xywh(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x0: x, y0: y, x1: x + w as i32, y1: y + h as i32 }
    }

    /// Smallest integer rect containing the float box.
    pub fn enclosing(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            x0: x0.min(x1).floor() as i32,
            y0: y0.min(y1).floor() as i32,
            x1: x0.max(x1).ceil() as i32,
            y1: y0.max(y1).ceil() as i32,
        }
    }

    pub fn width(&self) -> i32 {
        (self.x1 - self.x0).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.y1 - self.y0).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn intersect(&self, o: IRect) -> IRect {
        let r = IRect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        if r.is_empty() { IRect::EMPTY } else { r }
    }

    /// Bounding union; empty rects are ignored.
    pub fn union(&self, o: IRect) -> IRect {
        if self.is_empty() {
            return o;
        }
        if o.is_empty() {
            return *self;
        }
        IRect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    pub fn translate(&self, dx: i32, dy: i32) -> IRect {
        IRect { x0: self.x0 + dx, y0: self.y0 + dy, x1: self.x1 + dx, y1: self.y1 + dy }
    }

    pub fn expand(&self, n: i32) -> IRect {
        IRect { x0: self.x0 - n, y0: self.y0 - n, x1: self.x1 + n, y1: self.y1 + n }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub fn contains_rect(&self, o: IRect) -> bool {
        o.is_empty() || (o.x0 >= self.x0 && o.y0 >= self.y0 && o.x1 <= self.x1 && o.y1 <= self.y1)
    }
}
