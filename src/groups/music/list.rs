use std::collections::VecDeque;

use rand::{Rng, thread_rng, distributions::Uniform};

pub trait Shuffleable {
    fn shuffle(&mut self) -> &Self;
}

impl<T> Shuffleable for VecDeque<T> {
    fn shuffle(&mut self) -> &Self {
        let mut rng = thread_rng();
        let uniform = Uniform::<usize>::new(0, self.len());
        for i in 0..self.len() {
            self.swap(i, rng.sample(uniform));
        };
        self
    }
}

#[test]
fn shuffle_bound() {
    let mut vd = VecDeque::<usize>::new();
    vd.push_back(1);
    vd.push_back(4);
    for _ in 0..10516 {
        vd.shuffle();
    }
}