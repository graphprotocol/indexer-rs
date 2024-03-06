use eventuals::Eventual;

pub mod allocation_eligible;
pub mod deny_list_check;
pub mod sender_balance_check;

fn default_eventual<T: Send + Clone + Eq + 'static>() -> Eventual<T> {
    Eventual::new().1
}
