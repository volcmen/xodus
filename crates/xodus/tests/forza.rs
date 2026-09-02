mod forza {
    use xodus::forza::{Presence, XboxFriend, Xuid, build_invite_accept_uri, parse_friends};

    #[test]
    fn parses_friends_without_exposing_transport_fields() {
        let friends = parse_friends(include_str!("fixtures/friends.json")).unwrap();

        assert_eq!(
            friends,
            vec![XboxFriend {
                xuid: Xuid::new("2810000000000001").unwrap(),
                display_name: "Driver One".into(),
                presence: Presence::Online {
                    text: Some("Forza Motorsport".into()),
                },
            }]
        );
    }

    #[test]
    fn builds_validated_invite_accept_uri() {
        let uri = build_invite_accept_uri(
            &Xuid::new("2810000000000000").unwrap(),
            &Xuid::new("2810000000000001").unwrap(),
            "synthetic-connection",
        )
        .unwrap();

        assert!(
            uri.expose_for_activation()
                .starts_with("ms-xbl-multiplayer://inviteAccept?")
        );
        assert!(!format!("{uri:?}").contains("synthetic-connection"));
    }

    #[test]
    fn rejects_invalid_xuids_and_activation_uris() {
        for invalid in ["", "not-digits", "123456789012345678901"] {
            assert!(Xuid::new(invalid).is_err());
        }
        assert!(
            xodus::forza::InviteActivationUri::parse(
                "https://inviteAccept?invitedUser=2810000000000000&sender=2810000000000001&connectionString=synthetic"
            )
            .is_err()
        );
        assert!(
            xodus::forza::InviteActivationUri::parse(
                "ms-xbl-multiplayer://inviteAccept?invitedUser=2810000000000000&sender=2810000000000001"
            )
            .is_err()
        );
    }
}
