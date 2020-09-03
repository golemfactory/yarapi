#[macro_export]
macro_rules! expand_cmd {
    (deploy) => { $crate::requestor::Command::Deploy };
    (start) => { $crate::requestor::::Command::Start };
    (stop) => { $crate::requestor::::Command::Stop };
    (run ( $($e:expr),* )) => {{
        $crate::requestor::Command::Run(vec![ $($e.into()),* ])
    }};
    (transfer ( $e:expr, $f:expr )) => {
        $crate::requestor::Command::Transfer { from: $e.into(), to: $f.into() }
    };
    (upload ( $e:expr, $f:expr )) => {
        $crate::requestor::Command::Upload { from: $e.into(), to: $f.into() }
    };
    (download ( $e:expr, $f:expr )) => {
        $crate::requestor::Command::Download { from: $e.into(), to: $f.into() }
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
