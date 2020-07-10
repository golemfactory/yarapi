/* TODO don't use PaymentManager from gwasm-runner */
mod command;
mod package;
#[allow(dead_code)]
#[allow(unused_variables)]
#[allow(unused_must_use)]
mod payment_manager;
mod requestor;

pub use command::{Command, CommandList};
pub use package::Package;
pub use requestor::*;

#[macro_export]
macro_rules! expand_cmd {
    (deploy) => { $crate::Command::Deploy };
    (start) => { $crate::Command::Start };
    (stop) => { $crate::Command::Stop };
    (run ( $($e:expr),* )) => {{
        $crate::Command::Run(vec![ $($e.to_string()),* ])
    }};
    (transfer ( $e:expr, $f:expr )) => {
        $crate::Command::Transfer { from: $e.to_string(), to: $f.to_string() }
    };
    (upload ( $e:expr, $f:expr )) => {
        $crate::Command::Upload { from: $e.to_string(), to: $f.to_string() }
    };
    (download ( $e:expr, $f:expr )) => {
        $crate::Command::Download { from: $e.to_string(), to: $f.to_string() }
    };
}

#[macro_export]
macro_rules! commands_helper {
    () => {};
    ( $i:ident ( $($param:expr),* ) $(;)* ) => {{
        vec![$crate::expand_cmd!($i ( $($param),* ))]
    }};
    ( $i:tt $(;)* ) => {{
        vec![$crate::expand_cmd!($i)]
    }};
    ( $i:ident ( $($param:expr),* ) ; $( $t:tt )* ) => {{
        let mut tail = $crate::commands_helper!( $($t)* );
        tail.push($crate::expand_cmd!($i ( $($param),* )));
        tail
    }};
    ( $i:tt ; $( $t:tt )* ) => {{
        let mut tail = $crate::commands_helper!( $($t)* );
        tail.push($crate::expand_cmd!($i));
        tail
    }};
}

#[macro_export]
macro_rules! commands {
    ( $( $t:tt )* ) => {{
        let mut v = $crate::commands_helper!( $($t)* );
        v.reverse();
        CommandList::new(v)
    }};
}
