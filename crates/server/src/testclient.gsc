// Menu choices of a bot: pick a team, then a stock class each time it is about to spawn.
TestClient(team)
{
	self endon("disconnect");

	while (!isdefined(self.pers["team"]))
		wait 0.05;

	self notify("menuresponse", game["menu_team"], team);
	wait 0.5;

	names = getarraykeys(level.classMap);
	stock = [];
	for (i = 0; i < names.size; i++)
	{
		if (!issubstr(names[i], "custom") && isdefined(level.default_perk[level.classMap[names[i]]]))
			stock[stock.size] = names[i];
	}

	for (;;)
	{
		if (!level.oldschool)
			self notify("menuresponse", "changeclass", stock[randomint(stock.size)]);

		self waittill("spawned_player");
		wait 0.1;
	}
}
